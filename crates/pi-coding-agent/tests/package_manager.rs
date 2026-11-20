//! Port of packages/coding-agent/test/package-manager.test.ts

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use node_semver::Range;
use pi_coding_agent::core::package_manager::{
    DefaultPackageManager, InstalledSourceScope, MissingSourceAction, PackageCommandRunner, PackageManagerOptions,
    ParsedSource, ProgressAction, ProgressEvent, ProgressEventType,
};
use pi_coding_agent::core::settings_manager::{
    PackageFilterSource, PackageSource, SettingsManager, SettingsManagerCreateOptions,
};
use pi_coding_agent::core::source_info::{SourceOrigin, SourceScope};
use pi_js::env::testing::EnvGuard;
use pi_js::{Error, Result};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

struct PmFixture {
    _temp_dir: TempDir,
    root: PathBuf,
    agent_dir: String,
    cwd: String,
    settings_manager: SettingsManager,
    pm: DefaultPackageManager,
}

impl PmFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let root = temp_dir.path().to_path_buf();
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        fs::create_dir_all(&agent_dir).expect("mkdir agent");
        fs::create_dir_all(&cwd).expect("mkdir cwd");

        let settings_manager = SettingsManager::create(
            cwd.to_string_lossy().as_ref(),
            Some(agent_dir.to_string_lossy().as_ref()),
            Some(SettingsManagerCreateOptions {
                project_trusted: Some(true),
            }),
        );
        let pm = DefaultPackageManager::new(PackageManagerOptions {
            cwd: cwd.to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: Vec::new(),
        });
        Self {
            _temp_dir: temp_dir,
            root,
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            settings_manager,
            pm,
        }
    }

    fn write(path: &Path, content: &str) -> String {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir parents");
        }
        fs::write(path, content).expect("write test file");
        path.to_string_lossy().into_owned()
    }
}

#[derive(Clone, Debug)]
struct RecordedCommand {
    kind: &'static str,
    command: String,
    args: Vec<String>,
    _cwd: Option<String>,
}

type RunHook = Arc<dyn Fn(&str, &[&str], Option<&str>) -> Result<()> + Send + Sync>;
type CaptureHook = Arc<dyn Fn(&str, &[&str], Option<&str>, Option<u64>) -> Result<String> + Send + Sync>;
type SyncHook = Arc<dyn Fn(&str, &[&str]) -> Result<String> + Send + Sync>;

struct MockRunner {
    calls: Mutex<Vec<RecordedCommand>>,
    on_run: Option<RunHook>,
    on_capture: Option<CaptureHook>,
    on_sync: Option<SyncHook>,
}

impl MockRunner {
    fn with_hooks(on_run: Option<RunHook>, on_capture: Option<CaptureHook>, on_sync: Option<SyncHook>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            on_run,
            on_capture,
            on_sync,
        }
    }

    fn recorded(&self) -> Vec<RecordedCommand> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[async_trait::async_trait]
impl PackageCommandRunner for MockRunner {
    async fn run_command(&self, command: &str, args: &[&str], cwd: Option<&str>) -> Result<()> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RecordedCommand {
                kind: "run",
                command: command.to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                _cwd: cwd.map(str::to_owned),
            });
        if let Some(ref hook) = self.on_run {
            hook(command, args, cwd)
        } else {
            Ok(())
        }
    }

    async fn run_command_capture(
        &self,
        command: &str,
        args: &[&str],
        cwd: Option<&str>,
        timeout_ms: Option<u64>,
        _extra_env: Option<&IndexMap<String, String>>,
    ) -> Result<String> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RecordedCommand {
                kind: "capture",
                command: command.to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                _cwd: cwd.map(str::to_owned),
            });
        if let Some(ref hook) = self.on_capture {
            hook(command, args, cwd, timeout_ms)
        } else {
            Ok(String::new())
        }
    }

    fn run_command_sync(&self, command: &str, args: &[&str]) -> Result<String> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RecordedCommand {
                kind: "sync",
                command: command.to_owned(),
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                _cwd: None,
            });
        if let Some(ref hook) = self.on_sync {
            hook(command, args)
        } else {
            Ok(String::new())
        }
    }
}

#[test]
fn parses_npm_git_and_local_sources_and_identities() {
    let fx = PmFixture::new();

    match fx.pm.parse_source("npm:foo") {
        ParsedSource::Npm(npm) => {
            assert_eq!(npm.name, "foo");
            assert_eq!(npm.version, None);
            assert!(!npm.pinned);
        }
        other => panic!("expected npm, got {other:?}"),
    }
    match fx.pm.parse_source("npm:@scope/pkg@1.2.3") {
        ParsedSource::Npm(npm) => {
            assert_eq!(npm.name, "@scope/pkg");
            assert_eq!(npm.version.as_deref(), Some("1.2.3"));
            assert!(npm.pinned);
        }
        other => panic!("expected npm, got {other:?}"),
    }
    match fx.pm.parse_source("npm:@scope/pkg@^1.2.0") {
        ParsedSource::Npm(npm) => {
            assert_eq!(npm.name, "@scope/pkg");
            assert!(!npm.pinned);
            assert!(npm.range.is_some());
        }
        other => panic!("expected npm, got {other:?}"),
    }

    for url in [
        "https://github.com/user/repo",
        "git:https://github.com/user/repo",
        "https://github.com/user/repo@v1.2.3",
        "git:github.com/user/repo",
        "https://github.com/user/repo.git",
        "https://gitlab.com/user/repo",
        "https://bitbucket.org/user/repo",
        "https://codeberg.org/user/repo",
        "https://github.com/user/repo@feature/branch",
    ] {
        match fx.pm.parse_source(url) {
            ParsedSource::Git(git) => {
                assert_eq!(git.path, "user/repo");
                if url.contains('@') {
                    assert!(git.pinned);
                }
            }
            other => panic!("expected git for {url}, got {other:?}"),
        }
    }

    for local in ["/abs/path/pkg", "./packages/agent-timers", "../packages/agent-timers"] {
        assert!(matches!(fx.pm.parse_source(local), ParsedSource::Local(_)));
    }

    let id1 = fx
        .pm
        .get_package_identity("https://github.com/user/repo", None)
        .unwrap();
    let id2 = fx
        .pm
        .get_package_identity("https://github.com/user/repo@v1.0.0", None)
        .unwrap();
    let id3 = fx.pm.get_package_identity("git:github.com/user/repo", None).unwrap();
    let id4 = fx
        .pm
        .get_package_identity("https://github.com/user/repo.git", None)
        .unwrap();
    assert_eq!(id1, "git:github.com/user/repo");
    assert_eq!(id1, id2);
    assert_eq!(id1, id3);
    assert_eq!(id1, id4);
}

#[tokio::test]
async fn detects_package_manager_name_and_git_dependency_install_args() {
    let fx = PmFixture::new();
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "npm");
    assert_eq!(
        fx.pm.get_git_dependency_install_args().unwrap(),
        vec!["install", "--omit=dev", "--legacy-peer-deps", "--no-audit", "--no-fund"]
    );

    fx.settings_manager.set_npm_command(Some(vec!["pnpm.cmd".to_owned()]));
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "pnpm");
    assert_eq!(
        fx.pm.get_git_dependency_install_args().unwrap(),
        vec![
            "install",
            "--prod",
            "--config.auto-install-peers=false",
            "--config.strict-peer-dependencies=false",
            "--config.strict-dep-builds=false",
        ]
    );

    fx.settings_manager.set_npm_command(Some(vec!["bun.exe".to_owned()]));
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "bun");
    assert_eq!(
        fx.pm.get_git_dependency_install_args().unwrap(),
        vec!["install", "--omit=dev", "--omit=peer"]
    );

    fx.settings_manager.set_npm_command(Some(vec![
        "mise".to_owned(),
        "exec".to_owned(),
        "--".to_owned(),
        "pnpm".to_owned(),
    ]));
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "pnpm");

    fx.settings_manager
        .set_npm_command(Some(vec!["npx".to_owned(), "bun".to_owned()]));
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "bun");

    fx.settings_manager
        .set_npm_command(Some(vec!["wrapper".to_owned(), "pnpm".to_owned(), "bun".to_owned()]));
    let err = fx.pm.get_package_manager_name().unwrap_err().to_string();
    assert!(err.contains("Ambiguous npmCommand"));

    fx.settings_manager.set_npm_command(Some(vec!["custompm".to_owned()]));
    assert_eq!(fx.pm.get_package_manager_name().unwrap(), "custompm");
    assert_eq!(fx.pm.get_git_dependency_install_args().unwrap(), vec!["install"]);
}

#[tokio::test]
async fn discovers_local_manifest_resources_filters_and_deduplicates_scopes() {
    let fx = PmFixture::new();

    // Package A in global settings with manifest + exclusions
    let pkg_a = fx.root.join("pkg-a");
    let ext1 = PmFixture::write(&pkg_a.join("src/ext1.ts"), "// ext1");
    let ext2 = PmFixture::write(&pkg_a.join("src/ext2.ts"), "// ext2");
    let skill_ok = PmFixture::write(
        &pkg_a.join("skills/alpha/SKILL.md"),
        "---\nname: alpha\ndescription: alpha\n---\nbody",
    );
    let _prompt1 = PmFixture::write(&pkg_a.join("prompts/hello.md"), "hello");
    PmFixture::write(
        &pkg_a.join("package.json"),
        &pi_js::json::stringify(&json!({
            "name": "pkg-a",
            "pi": {
                "extensions": ["src"],
                "skills": ["skills"],
                "prompts": ["prompts"]
            }
        })),
    );

    // Same local package identity in project settings with filter disabling ext2
    // (project package paths resolve relative to <cwd>/.pi)
    fx.settings_manager
        .set_packages(vec![PackageSource::String(pkg_a.to_string_lossy().into_owned())]);
    let _ = fx
        .settings_manager
        .set_project_packages(vec![PackageSource::Object(PackageFilterSource {
            source: "../../pkg-a".to_owned(),
            autoload: None,
            extensions: Some(vec!["!**/ext2.ts".to_owned()]),
            skills: None,
            prompts: None,
            themes: None,
        })]);

    // Untested auto-discovery with .gitignore negation
    let pkg_b = fx.root.join("pkg-b");
    let keep_ext = PmFixture::write(&pkg_b.join("extensions/kept.ts"), "// kept");
    let _ignored_ext = PmFixture::write(&pkg_b.join("extensions/ignored.ts"), "// ignored");
    PmFixture::write(&pkg_b.join("extensions/.gitignore"), "*.ts\n!kept.ts\n");
    fx.pm
        .add_source_to_settings(&pkg_b.to_string_lossy(), false)
        .expect("add pkg-b");

    // Autoload false package excluded unless resolved manually
    let pkg_c = fx.root.join("pkg-c");
    let manual_ext = PmFixture::write(&pkg_c.join("extensions/manual.ts"), "// manual");
    let _ = fx.settings_manager.set_project_packages(vec![
        PackageSource::Object(PackageFilterSource {
            source: "../../pkg-a".to_owned(),
            autoload: None,
            extensions: Some(vec!["!**/ext2.ts".to_owned()]),
            skills: None,
            prompts: None,
            themes: None,
        }),
        PackageSource::Object(PackageFilterSource {
            source: pkg_c.to_string_lossy().into_owned(),
            autoload: Some(false),
            extensions: None,
            skills: None,
            prompts: None,
            themes: None,
        }),
    ]);

    let resolved = fx.pm.resolve(None).await.expect("resolve");
    let ext_map: IndexMap<String, bool> = resolved
        .extensions
        .iter()
        .map(|r| (r.path.clone(), r.enabled))
        .collect();
    assert_eq!(ext_map.get(&ext1), Some(&true));
    assert_eq!(ext_map.get(&ext2), Some(&false));
    assert_eq!(ext_map.get(&keep_ext), Some(&true));
    assert!(!ext_map.contains_key(&manual_ext));
    assert!(resolved.skills.iter().any(|r| r.path == skill_ok && r.enabled));

    // Resolve manual/temporary path still works
    let cli_resolved = fx
        .pm
        .resolve_extension_sources(&[manual_ext.clone()], false, true)
        .await
        .expect("resolve extension sources");
    assert_eq!(cli_resolved.extensions.len(), 1);
    assert_eq!(cli_resolved.extensions[0].path, manual_ext);
    assert_eq!(cli_resolved.extensions[0].metadata.scope, SourceScope::Temporary);
    assert_eq!(cli_resolved.extensions[0].metadata.origin, SourceOrigin::Package);
}

#[tokio::test]
async fn enforces_project_trust_and_resolves_missing_callback_actions() {
    let fx = PmFixture::new();
    let runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(move |cmd, args, _cwd| {
            // Simulate `npm install` creating the installed package manifest on disk.
            if cmd == "npm" && args.first() == Some(&"install") {
                if let Some(prefix_idx) = args.iter().position(|a| *a == "--prefix")
                    && let Some(root) = args.get(prefix_idx + 1)
                {
                    let pkg_dir = Path::new(root).join("node_modules/fake-pkg");
                    PmFixture::write(
                        &pkg_dir.join("package.json"),
                        r#"{"name":"fake-pkg","version":"1.0.0"}"#,
                    );
                    PmFixture::write(&pkg_dir.join("extensions/index.ts"), "// ext");
                }
            }
            Ok(())
        })),
        None,
        None,
    ));
    fx.pm.set_command_runner(Some(runner));

    // Skip missing source when callback returns Skip
    fx.settings_manager
        .set_packages(vec![PackageSource::String("npm:fake-pkg".to_owned())]);
    let skipped = fx
        .pm
        .resolve(Some(Arc::new(|_| Box::pin(async { Ok(MissingSourceAction::Skip) }))))
        .await
        .expect("skip missing");
    assert!(skipped.extensions.is_empty());

    // Error missing source when callback returns Error
    let err = fx
        .pm
        .resolve(Some(Arc::new(|_| Box::pin(async { Ok(MissingSourceAction::Error) }))))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Missing source"));

    // Install missing source when callback returns Install
    let events = Arc::new(Mutex::new(Vec::<ProgressEvent>::new()));
    let events_ref = events.clone();
    fx.pm.set_progress_callback(Some(Arc::new(move |ev| {
        events_ref.lock().unwrap().push(ev);
    })));
    let installed = fx
        .pm
        .resolve(Some(Arc::new(|_| Box::pin(async { Ok(MissingSourceAction::Install) }))))
        .await
        .expect("install missing");
    assert_eq!(installed.extensions.len(), 1);
    assert_eq!(
        fx.pm
            .get_installed_path("npm:fake-pkg", InstalledSourceScope::User)
            .unwrap()
            .as_deref()
            .map(|p| Path::new(p).ends_with("agent/npm/node_modules/fake-pkg")),
        Some(true)
    );

    fx.pm
        .install("npm:fake-pkg", false)
        .await
        .expect("install emits progress");
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| { e.event_type == ProgressEventType::Start && e.action == ProgressAction::Install })
    );

    // Untrusted project refuses project-scoped operations
    fx.settings_manager.set_project_trusted(false);
    let err = fx.pm.install("npm:other", true).await.unwrap_err().to_string();
    assert!(err.contains("Project is not trusted"));
    assert!(
        fx.pm
            .get_installed_path("npm:other", InstalledSourceScope::Project)
            .is_err()
    );
}

#[tokio::test]
#[serial]
async fn handles_npm_install_update_pinned_offline_legacy_root_and_latest_version() {
    let _offline_clear = EnvGuard::set("PI_OFFLINE", None);
    let fx = PmFixture::new();
    let legacy_root = fx.root.join("legacy-global-npm");
    PmFixture::write(
        &legacy_root.join("legacy-pkg/package.json"),
        r#"{"name":"legacy-pkg","version":"1.0.0"}"#,
    );

    let view_response = Arc::new(Mutex::new(r#""1.9.0""#.to_owned()));
    let view_resp_ref = view_response.clone();
    let legacy_root_str = legacy_root.to_string_lossy().into_owned();

    let runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(|_cmd, _args, _cwd| Ok(()))),
        Some(Arc::new(move |cmd, args, _cwd, _timeout| {
            if cmd == "npm" && args.first() == Some(&"view") {
                return Ok(view_resp_ref.lock().unwrap().clone());
            }
            Ok(String::new())
        })),
        Some(Arc::new(move |cmd, args| {
            if cmd == "npm" && args == ["root", "-g"] {
                return Ok(legacy_root_str.clone());
            }
            Ok(String::new())
        })),
    ));
    fx.pm.set_command_runner(Some(runner.clone()));

    // Legacy global npm package found and updated when newer version available
    let ParsedSource::Npm(parsed_legacy) = fx.pm.parse_source("npm:legacy-pkg") else {
        panic!("expected npm");
    };
    assert_eq!(
        fx.pm.get_npm_install_path(&parsed_legacy, SourceScope::User).unwrap(),
        legacy_root.join("legacy-pkg").to_string_lossy()
    );
    fx.settings_manager
        .set_packages(vec![PackageSource::String("npm:legacy-pkg".to_owned())]);
    fx.pm.update(None).await.expect("update legacy");
    assert!(
        runner
            .recorded()
            .iter()
            .any(|c| { c.kind == "run" && c.command == "npm" && c.args.iter().any(|a| a.starts_with("legacy-pkg")) })
    );

    // Managed installed package whose version already satisfies range is NOT reinstalled unless outdated
    let managed_dir = Path::new(&fx.agent_dir).join("npm/node_modules/sat-pkg");
    PmFixture::write(
        &managed_dir.join("package.json"),
        r#"{"name":"sat-pkg","version":"1.5.0"}"#,
    );
    let ParsedSource::Npm(sat_parsed) = fx.pm.parse_source("npm:sat-pkg@^1.0.0") else {
        panic!("expected npm");
    };
    assert!(
        fx.pm
            .installed_npm_matches_configured_version(&sat_parsed, managed_dir.to_str().unwrap())
    );
    *view_response.lock().unwrap() = r#""1.5.0""#.to_owned();
    fx.settings_manager
        .set_packages(vec![PackageSource::String("npm:sat-pkg@^1.0.0".to_owned())]);
    let calls_before = runner.recorded().len();
    fx.pm.update(None).await.expect("no-op up-to-date");
    let runs_during = runner.recorded()[calls_before..]
        .iter()
        .filter(|c| c.kind == "run")
        .count();
    assert_eq!(runs_during, 0);

    // Pinned npm version skips update altogether
    fx.settings_manager
        .set_packages(vec![PackageSource::String("npm:sat-pkg@1.5.0".to_owned())]);
    let calls_before_pinned = runner.recorded().len();
    fx.pm.update(None).await.expect("skip pinned");
    assert_eq!(runner.recorded().len(), calls_before_pinned);

    // Offline mode skips npm view update checks
    {
        let _offline = EnvGuard::set("PI_OFFLINE", Some("1"));
        fx.settings_manager
            .set_packages(vec![PackageSource::String("npm:sat-pkg".to_owned())]);
        let calls_before_offline = runner.recorded().len();
        fx.pm.update(None).await.expect("offline skips network");
        assert_eq!(runner.recorded().len(), calls_before_offline);
    }

    // get_latest_npm_version parses single string and array filtered by semver Range
    *view_response.lock().unwrap() = r#"["1.0.0","1.4.0","2.1.0"]"#.to_owned();
    let range: Range = "^1.0.0".parse().unwrap();
    let latest_v1 = fx.pm.get_latest_npm_version("example", Some(&range)).await.unwrap();
    assert_eq!(latest_v1, "1.4.0");

    // Failing npm command propagates error
    let failing_runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(|_, _, _| Err(Error::msg("npm failed")))),
        None,
        None,
    ));
    fx.pm.set_command_runner(Some(failing_runner));
    assert!(fx.pm.install("npm:broken-pkg", false).await.is_err());

    // pnpm global list parsing + invalid JSON handling
    fx.settings_manager.set_npm_command(Some(vec!["pnpm".to_owned()]));
    let pnpm_dir = fx.root.join("pnpm-global/pnpm-pkg");
    PmFixture::write(&pnpm_dir.join("package.json"), r#"{"version":"1.0.0"}"#);
    let pnpm_json = pi_js::json::stringify(&json!([{
        "dependencies": {
            "pnpm-pkg": { "path": pnpm_dir.to_string_lossy() }
        }
    }]));
    let pnpm_runner = Arc::new(MockRunner::with_hooks(
        None,
        None,
        Some(Arc::new(move |cmd, args| {
            if cmd == "pnpm" && args == ["list", "-g", "--depth", "0", "--json"] {
                return Ok(pnpm_json.clone());
            }
            Ok(String::new())
        })),
    ));
    fx.pm.set_command_runner(Some(pnpm_runner));
    let ParsedSource::Npm(pnpm_parsed) = fx.pm.parse_source("npm:pnpm-pkg") else {
        panic!("expected npm");
    };
    assert_eq!(
        fx.pm.get_npm_install_path(&pnpm_parsed, SourceScope::User).unwrap(),
        pnpm_dir.to_string_lossy()
    );
}

#[tokio::test]
async fn updates_and_installs_git_packages_with_mocks_and_real_local_repo() {
    let fx = PmFixture::new();

    // 1. Mocked git flow: clone + ref checkout + update checking remote vs head + skip pinned
    let repo_install = fx
        .pm
        .get_git_install_path(
            match fx.pm.parse_source("git:github.com/example/repo") {
                ParsedSource::Git(ref g) => g,
                _ => unreachable!(),
            },
            SourceScope::User,
        )
        .unwrap();
    PmFixture::write(&Path::new(&repo_install).join(".git/HEAD"), "ref: refs/heads/main\n");
    PmFixture::write(
        &Path::new(&repo_install).join("package.json"),
        r#"{"name":"repo","version":"1.0.0"}"#,
    );

    let git_runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(|_, _, _| Ok(()))),
        Some(Arc::new(|cmd, args, _cwd, _timeout| {
            if cmd == "git" && args.first() == Some(&"rev-parse") && args.get(1) == Some(&"HEAD") {
                return Ok("1111111111111111111111111111111111111111".to_owned());
            }
            if cmd == "git" && args.first() == Some(&"ls-remote") {
                return Ok(
                    "2222222222222222222222222222222222222222\tHEAD\n2222222222222222222222222222222222222222\trefs/heads/main"
                        .to_owned(),
                );
            }
            if cmd == "git" && args.first() == Some(&"symbolic-ref") && args.contains(&"refs/remotes/origin/HEAD") {
                return Ok("refs/remotes/origin/main".to_owned());
            }
            if cmd == "git" && args.first() == Some(&"rev-parse") {
                return Ok("2222222222222222222222222222222222222222".to_owned());
            }
            Ok(String::new())
        })),
        None,
    ));
    fx.pm.set_command_runner(Some(git_runner.clone()));
    fx.settings_manager
        .set_packages(vec![PackageSource::String("git:github.com/example/repo".to_owned())]);
    fx.pm.update(None).await.expect("git update");
    assert!(
        git_runner
            .recorded()
            .iter()
            .any(|c| { c.kind == "run" && c.command == "git" && c.args.first().map(String::as_str) == Some("fetch") })
    );

    // 2. Timeout behavior on real default_run_command_capture
    let err =
        DefaultPackageManager::default_run_command_capture("sh", &["-c", "sleep 2"], Some(&fx.cwd), Some(50), None)
            .await
            .unwrap_err()
            .to_string();
    assert!(err.contains("timed out after 50ms"), "got: {err}");

    // 3. Real local bare git repository clone + update using default command runner
    fx.pm.set_command_runner(None);
    let seed_repo = fx.root.join("seed-repo");
    let bare_repo = fx.root.join("bare-repo.git");
    fs::create_dir_all(&seed_repo).unwrap();
    let run_git = |cwd: &Path, args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&seed_repo, &["init", "-b", "main"]);
    PmFixture::write(&seed_repo.join("extensions/local-git.ts"), "// v1");
    run_git(&seed_repo, &["add", "."]);
    run_git(&seed_repo, &["commit", "-m", "v1"]);
    run_git(
        &fx.root,
        &[
            "clone",
            "--bare",
            seed_repo.to_str().unwrap(),
            bare_repo.to_str().unwrap(),
        ],
    );

    let clone_dest = fx.root.join("cloned-dest");
    DefaultPackageManager::default_run_command(
        "git",
        &[
            "clone",
            &format!("file://{}", bare_repo.display()),
            clone_dest.to_str().unwrap(),
        ],
        None,
    )
    .await
    .expect("clone file:// repo");

    // Push a second commit to bare repo and verify rev-parse update check
    PmFixture::write(&seed_repo.join("extensions/local-git.ts"), "// v2");
    run_git(&seed_repo, &["add", "."]);
    run_git(&seed_repo, &["commit", "-m", "v2"]);
    run_git(&seed_repo, &["push", bare_repo.to_str().unwrap(), "main"]);

    let mut remote_head_out = DefaultPackageManager::default_run_command_capture(
        "git",
        &["rev-parse", "refs/heads/main"],
        Some(bare_repo.to_str().unwrap()),
        Some(5_000),
        None,
    )
    .await
    .unwrap();
    remote_head_out = remote_head_out.trim().to_owned();
    let local_head_before = DefaultPackageManager::default_run_command_capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(clone_dest.to_str().unwrap()),
        Some(5_000),
        None,
    )
    .await
    .unwrap();
    assert_ne!(local_head_before, remote_head_out);

    let list = fx.pm.list_configured_packages().unwrap();
    assert!(!list.is_empty());
    let _: Value = json!({});
}

#[tokio::test]
async fn resolves_changelog_path_metadata_and_ignores_parent_traversal() {
    let fx = PmFixture::new();

    // 1. Manifest changelogPath
    let pkg1 = fx.root.join("changelog-manifest-pkg");
    let ext1 = PmFixture::write(&pkg1.join("src/index.ts"), "export default function() {}");
    let ch1 = PmFixture::write(&pkg1.join("docs/CHANGELOG.md"), "## [1.0.0]\n\n- Initial release");
    PmFixture::write(
        &pkg1.join("package.json"),
        &json!({
            "name": "changelog-manifest-pkg",
            "pi": {
                "extensions": ["src/index.ts"],
                "changelogPath": "docs/CHANGELOG.md"
            }
        })
        .to_string(),
    );
    let res1 = fx
        .pm
        .resolve_extension_sources(&[pkg1.to_string_lossy().into_owned()], false, false)
        .await
        .unwrap();
    let e1 = res1.extensions.iter().find(|r| r.path == ext1).unwrap();
    assert_eq!(e1.metadata.changelog_path.as_deref(), Some(ch1.as_str()));

    // 2. Convention CHANGELOG.md
    let pkg2 = fx.root.join("changelog-convention-pkg");
    let ext2 = PmFixture::write(&pkg2.join("extensions/index.ts"), "export default function() {}");
    let ch2 = PmFixture::write(&pkg2.join("CHANGELOG.md"), "## [1.0.0]\n\n- Initial release");
    let res2 = fx
        .pm
        .resolve_extension_sources(&[pkg2.to_string_lossy().into_owned()], false, false)
        .await
        .unwrap();
    let e2 = res2.extensions.iter().find(|r| r.path == ext2).unwrap();
    assert_eq!(e2.metadata.changelog_path.as_deref(), Some(ch2.as_str()));

    // 3. Auto-discover extensions when manifest only declares changelogPath
    let pkg3 = fx.root.join("changelog-only-manifest-pkg");
    let ext3 = PmFixture::write(&pkg3.join("extensions/index.ts"), "export default function() {}");
    let ch3 = PmFixture::write(&pkg3.join("CHANGELOG.md"), "## [1.0.0]\n\n- Initial release");
    PmFixture::write(
        &pkg3.join("package.json"),
        &json!({
            "name": "changelog-only-manifest-pkg",
            "pi": { "changelogPath": "CHANGELOG.md" }
        })
        .to_string(),
    );
    let res3 = fx
        .pm
        .resolve_extension_sources(&[pkg3.to_string_lossy().into_owned()], false, false)
        .await
        .unwrap();
    let e3 = res3.extensions.iter().find(|r| r.path == ext3).unwrap();
    assert_eq!(e3.metadata.changelog_path.as_deref(), Some(ch3.as_str()));

    // 4. Rejects changelogPath escaping package root
    PmFixture::write(&fx.root.join("CHANGELOG.md"), "## [1.0.0]\n\n- Outside");
    let pkg4 = fx.root.join("unsafe-changelog-pkg");
    let ext4 = PmFixture::write(&pkg4.join("src/index.ts"), "export default function() {}");
    PmFixture::write(
        &pkg4.join("CHANGELOG.md"),
        "## [1.0.0]\n\n- Fallback should not be used",
    );
    PmFixture::write(
        &pkg4.join("package.json"),
        &json!({
            "name": "unsafe-changelog-pkg",
            "pi": {
                "extensions": ["src/index.ts"],
                "changelogPath": "../CHANGELOG.md"
            }
        })
        .to_string(),
    );
    let res4 = fx
        .pm
        .resolve_extension_sources(&[pkg4.to_string_lossy().into_owned()], false, false)
        .await
        .unwrap();
    let e4 = res4.extensions.iter().find(|r| r.path == ext4).unwrap();
    assert_eq!(e4.metadata.changelog_path, None);
}

#[cfg(unix)]
#[tokio::test]
async fn passes_realpath_prefix_when_agent_dir_is_symlink_11358() {
    let tmp = tempfile::tempdir().unwrap();
    let real_agent = tmp.path().join("dotfiles/agent");
    let linked_agent = tmp.path().join("linked-agent");
    fs::create_dir_all(&real_agent).unwrap();
    std::os::unix::fs::symlink(&real_agent, &linked_agent).unwrap();

    let cwd = tmp.path().join("project");
    fs::create_dir_all(&cwd).unwrap();
    let settings = SettingsManager::create(
        cwd.to_string_lossy().as_ref(),
        Some(linked_agent.to_string_lossy().as_ref()),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(true),
        }),
    );
    let pm = DefaultPackageManager::new(PackageManagerOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: linked_agent.to_string_lossy().into_owned(),
        settings_manager: settings,
        builtin_extensions: Vec::new(),
    });
    let runner = Arc::new(MockRunner::with_hooks(None, None, None));
    pm.set_command_runner(Some(Arc::clone(&runner) as Arc<dyn PackageCommandRunner>));

    pm.install("npm:@scope/pkg", false).await.unwrap();
    pm.remove("npm:@scope/pkg", false).await.unwrap();

    let real_npm_root = fs::canonicalize(&real_agent).unwrap().join("npm");
    let real_prefix = real_npm_root.to_string_lossy().into_owned();
    let calls = runner.recorded();
    assert_eq!(
        calls[0].args,
        vec![
            "install",
            "@scope/pkg",
            "--prefix",
            &real_prefix,
            "--legacy-peer-deps",
            "--no-audit",
            "--no-fund"
        ]
    );
    assert_eq!(
        calls[1].args,
        vec![
            "uninstall",
            "@scope/pkg",
            "--prefix",
            &real_prefix,
            "--legacy-peer-deps",
            "--no-audit",
            "--no-fund"
        ]
    );
}

#[tokio::test]
#[serial]
async fn expands_tilde_in_exclusion_and_exact_patterns_10765() {
    let fx = PmFixture::new();
    let home = fx.root.to_string_lossy().into_owned();
    let _home_guard = EnvGuard::set("HOME", Some(&home));

    for name in ["keep", "glob-excluded", "exact-excluded"] {
        PmFixture::write(
            &fx.root.join(format!("extra/skills/{name}/SKILL.md")),
            &format!("---\nname: {name}\ndescription: {name}\n---\n"),
        );
    }
    fx.settings_manager.set_skill_paths(vec![
        "~/extra/skills".to_owned(),
        "!~/extra/skills/glob-*".to_owned(),
        "-~/extra/skills/exact-excluded".to_owned(),
    ]);

    let result = fx.pm.resolve(None).await.unwrap();
    assert!(
        result
            .skills
            .iter()
            .any(|r| r.path.ends_with("keep/SKILL.md") && r.enabled)
    );
    assert!(
        result
            .skills
            .iter()
            .any(|r| r.path.ends_with("glob-excluded/SKILL.md") && !r.enabled)
    );
    assert!(
        result
            .skills
            .iter()
            .any(|r| r.path.ends_with("exact-excluded/SKILL.md") && !r.enabled)
    );
}

#[tokio::test]
async fn records_install_failures_without_aborting_resolve_11088() {
    let fx = PmFixture::new();
    let runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(|_, _, _| {
            Err(Error::msg("npm install broken-package failed with code 1"))
        })),
        None,
        None,
    ));
    fx.pm.set_command_runner(Some(runner));
    let _ = fx
        .settings_manager
        .set_project_packages(vec![PackageSource::String("npm:broken-package".to_owned())]);

    let result = fx.pm.resolve(None).await.unwrap();
    let failures = result.install_failures.unwrap_or_default();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].source, "npm:broken-package");
    assert_eq!(failures[0].scope, SourceScope::Project);
    assert!(
        failures[0]
            .message
            .contains("npm install broken-package failed with code 1")
    );
    assert!(result.extensions.is_empty());
}

#[tokio::test]
#[serial]
async fn emits_update_result_and_stale_warning_progress_events() {
    let _offline_clear = EnvGuard::set("PI_OFFLINE", None);
    let fx = PmFixture::new();

    // 1. Emit Start -> Complete -> Result for version upgrade
    let installed_path = Path::new(&fx.cwd).join(".pi/npm/node_modules/example");
    PmFixture::write(
        &installed_path.join("package.json"),
        r#"{"name":"example","version":"1.0.0"}"#,
    );
    let _ = fx
        .settings_manager
        .set_project_packages(vec![PackageSource::String("npm:example@^1.0.0".to_owned())]);

    let inst_c = installed_path.clone();
    let runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(move |_, _, _| {
            PmFixture::write(&inst_c.join("package.json"), r#"{"name":"example","version":"1.2.0"}"#);
            Ok(())
        })),
        Some(Arc::new(|_, _, _, _| Ok(r#"["1.0.0","1.2.0"]"#.to_owned()))),
        None,
    ));
    fx.pm.set_command_runner(Some(runner));
    let events = Arc::new(Mutex::new(Vec::<ProgressEvent>::new()));
    let events_c = Arc::clone(&events);
    fx.pm.set_progress_callback(Some(Arc::new(move |ev| {
        events_c.lock().unwrap().push(ev);
    })));
    fx.pm.update(Some("npm:example")).await.unwrap();
    let recorded = events.lock().unwrap().clone();
    assert_eq!(
        recorded
            .iter()
            .map(|e| (e.event_type, e.source.as_str(), e.message.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (
                ProgressEventType::Start,
                "npm:example@^1.0.0",
                Some("Updating npm:example@^1.0.0...")
            ),
            (ProgressEventType::Complete, "npm:example@^1.0.0", None),
            (
                ProgressEventType::Result,
                "npm:example@^1.0.0",
                Some("npm:example@^1.0.0 (project) 1.0.0 -> 1.2.0")
            ),
        ]
    );

    // 2. Warn when npm update leaves installed version unchanged
    let fx2 = PmFixture::new();
    let stale_path = Path::new(&fx2.agent_dir).join("npm/node_modules/stale-pkg");
    let fresh_path = Path::new(&fx2.agent_dir).join("npm/node_modules/fresh-pkg");
    PmFixture::write(
        &stale_path.join("package.json"),
        r#"{"name":"stale-pkg","version":"1.0.0"}"#,
    );
    PmFixture::write(
        &fresh_path.join("package.json"),
        r#"{"name":"fresh-pkg","version":"1.0.0"}"#,
    );
    fx2.settings_manager.set_packages(vec![
        PackageSource::String("npm:stale-pkg".to_owned()),
        PackageSource::String("npm:fresh-pkg".to_owned()),
    ]);
    let fresh_c = fresh_path.clone();
    let runner2 = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(move |_, _, _| {
            PmFixture::write(
                &fresh_c.join("package.json"),
                r#"{"name":"fresh-pkg","version":"1.0.1"}"#,
            );
            Ok(())
        })),
        Some(Arc::new(|_, _, _, _| Ok(r#""1.0.1""#.to_owned()))),
        None,
    ));
    fx2.pm.set_command_runner(Some(runner2));
    let events2 = Arc::new(Mutex::new(Vec::<ProgressEvent>::new()));
    let events2_c = Arc::clone(&events2);
    fx2.pm.set_progress_callback(Some(Arc::new(move |ev| {
        events2_c.lock().unwrap().push(ev);
    })));
    fx2.pm.update(None).await.unwrap();
    let warnings: Vec<ProgressEvent> = events2
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.event_type == ProgressEventType::Progress)
        .cloned()
        .collect();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].source, "npm:stale-pkg");
    let msg = warnings[0].message.as_deref().unwrap_or("");
    assert!(msg.contains("is still 1.0.0") && msg.contains("1.0.1"), "msg={msg}");
}

#[tokio::test]
#[serial]
async fn refreshes_temporary_git_and_npm_sources_cleanly_11375() {
    let _offline_clear = EnvGuard::set("PI_OFFLINE", None);
    let fx = PmFixture::new();

    // 1. Branch-pinned temporary git source refreshes (fetches/checks out)
    let git_branch_src = "git:github.com/example/repo@main";
    let ParsedSource::Git(ref git_branch) = fx.pm.parse_source(git_branch_src) else {
        panic!("expected git");
    };
    let git_temp = fx.pm.get_git_install_path(git_branch, SourceScope::Temporary).unwrap();
    PmFixture::write(
        &Path::new(&git_temp).join("extensions/index.ts"),
        "export default function() {};",
    );
    let git_runner = Arc::new(MockRunner::with_hooks(None, None, None));
    fx.pm
        .set_command_runner(Some(Arc::clone(&git_runner) as Arc<dyn PackageCommandRunner>));
    let res = fx
        .pm
        .resolve_extension_sources(&[git_branch_src.to_owned()], false, true)
        .await
        .unwrap();
    assert!(
        res.extensions
            .iter()
            .any(|r| r.path.ends_with("extensions/index.ts") && r.enabled)
    );
    assert!(
        git_runner
            .recorded()
            .iter()
            .any(|c| c.kind == "run" && c.command == "git" && c.args.first().map(String::as_str) == Some("fetch"))
    );

    // 2. Commit-SHA temporary git source whose checked-out HEAD matches ref skips update
    let sha_src = "git:github.com/example/repo@abc1234";
    let ParsedSource::Git(ref git_sha) = fx.pm.parse_source(sha_src) else {
        panic!("expected git");
    };
    let sha_temp = fx.pm.get_git_install_path(git_sha, SourceScope::Temporary).unwrap();
    fs::create_dir_all(&sha_temp).unwrap();
    let sha_runner = Arc::new(MockRunner::with_hooks(
        None,
        Some(Arc::new(|cmd, args, _, _| {
            if cmd == "git" && args.first() == Some(&"rev-parse") {
                return Ok("ABC1234def5678901234567890123456789012345\n".to_owned());
            }
            Ok(String::new())
        })),
        None,
    ));
    fx.pm
        .set_command_runner(Some(Arc::clone(&sha_runner) as Arc<dyn PackageCommandRunner>));
    let _ = fx
        .pm
        .resolve_extension_sources(&[sha_src.to_owned()], false, true)
        .await
        .unwrap();
    assert!(!sha_runner.recorded().iter().any(|c| c.kind == "run"));

    // 3. Unpinned temporary npm source refreshes when newer version exists, keeping cached install on failure
    let npm_src = "npm:example";
    let ParsedSource::Npm(ref npm_parsed) = fx.pm.parse_source(npm_src) else {
        panic!("expected npm");
    };
    let npm_temp = fx.pm.get_npm_install_path(npm_parsed, SourceScope::Temporary).unwrap();
    PmFixture::write(
        &Path::new(&npm_temp).join("package.json"),
        r#"{"name":"example","version":"1.0.0"}"#,
    );
    PmFixture::write(
        &Path::new(&npm_temp).join("extensions/index.ts"),
        "export default function() {};",
    );
    let npm_runner = Arc::new(MockRunner::with_hooks(
        Some(Arc::new(|_, _, _| Err(Error::msg("network down")))),
        Some(Arc::new(|cmd, args, _, _| {
            if cmd == "npm" && args.first() == Some(&"view") {
                return Ok(r#""1.1.0""#.to_owned());
            }
            Ok(String::new())
        })),
        None,
    ));
    fx.pm
        .set_command_runner(Some(Arc::clone(&npm_runner) as Arc<dyn PackageCommandRunner>));
    let npm_res = fx
        .pm
        .resolve_extension_sources(&[npm_src.to_owned()], false, true)
        .await
        .unwrap();
    assert!(
        npm_res
            .extensions
            .iter()
            .any(|r| r.path.ends_with("extensions/index.ts") && r.enabled)
    );
    assert!(
        npm_runner
            .recorded()
            .iter()
            .any(|c| c.kind == "run" && c.args.iter().any(|a| a == "example@latest"))
    );
}

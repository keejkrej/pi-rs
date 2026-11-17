//! Port of packages/coding-agent/test/git-update.test.ts

mod support;

use std::process::Command;
use std::sync::{Arc, Mutex};

use pi_coding_agent::core::settings_manager::{PackageSource, SettingsManager};
use pi_coding_agent::utils::git::{GitSource, parse_git_url};
use pi_js::{Error, Result};
use tempfile::TempDir;

use support::test_network_env::allow_network;

fn git(args: &[&str], cwd: &str) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git invocation");
    if !out.status.success() {
        panic!(
            "Command failed: git {}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn init_git_repo(repo_dir: &str) {
    git(&["init", "--initial-branch=main"], repo_dir);
    git(&["config", "--local", "user.email", "test@test.com"], repo_dir);
    git(&["config", "--local", "user.name", "Test"], repo_dir);
}

fn create_commit(repo_dir: &str, filename: &str, content: &str, message: &str) -> String {
    let file_path = pi_js::path::join(&[repo_dir, filename]);
    pi_js::fs::write_file(&file_path, content, None).expect("write commit file");
    git(&["add", filename], repo_dir);
    git(&["commit", "-m", message], repo_dir);
    git(&["rev-parse", "HEAD"], repo_dir)
}

fn get_current_commit(repo_dir: &str) -> String {
    git(&["rev-parse", "HEAD"], repo_dir)
}

fn get_file_content(repo_dir: &str, filename: &str) -> String {
    pi_js::fs::read_to_string(&pi_js::path::join(&[repo_dir, filename])).expect("read file content")
}

// WIRE(@mcp-helper2): replace local `TestPackageManager` adapter with
// `pi_coding_agent::core::package_manager::DefaultPackageManager` once `core-models-pkg` lands.
type CommandHook = Arc<dyn Fn(&str, &[&str], Option<&str>) -> Result<()> + Send + Sync>;
type CaptureHook = Arc<dyn Fn(&str, &[&str], Option<&str>) -> Result<String> + Send + Sync>;

struct TestPackageManager {
    agent_dir: String,
    settings_manager: SettingsManager,
    run_cmd: CommandHook,
    run_capture: CaptureHook,
}

impl TestPackageManager {
    fn new(_cwd: &str, agent_dir: &str, settings_manager: SettingsManager) -> Self {
        let run_cmd: CommandHook = Arc::new(|command, args, cwd| {
            let mut cmd = Command::new(command);
            cmd.args(args);
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            let out = cmd
                .output()
                .map_err(|e| Error::msg(format!("Command failed: {command} {}: {e}", args.join(" "))))?;
            if !out.status.success() {
                return Err(Error::msg(format!(
                    "Command failed: {command} {}\n{}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(())
        });
        let run_capture: CaptureHook = Arc::new(|command, args, cwd| {
            let mut cmd = Command::new(command);
            cmd.args(args);
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            let out = cmd
                .output()
                .map_err(|e| Error::msg(format!("Command failed: {command} {}: {e}", args.join(" "))))?;
            if !out.status.success() {
                return Err(Error::msg(format!(
                    "Command failed: {command} {}\n{}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        });
        Self {
            agent_dir: agent_dir.to_owned(),
            settings_manager,
            run_cmd,
            run_capture,
        }
    }

    fn parse_source(&self, source: &str) -> Result<GitSource> {
        parse_git_url(source).ok_or_else(|| Error::msg(format!("Invalid git source: {source}")))
    }

    fn get_git_install_path(&self, source: &GitSource, scope: &str) -> String {
        let base = if scope == "temporary" {
            pi_js::path::join(&[&self.agent_dir, "tmp-git"])
        } else {
            pi_js::path::join(&[&self.agent_dir, "git"])
        };
        pi_js::path::join(&[&base, &source.host, &source.path])
    }

    fn refresh_git_dir(&self, install_dir: &str, ref_name: Option<&str>) -> Result<()> {
        if let Some(r) = ref_name {
            (self.run_cmd)("git", &["fetch", "--prune", "origin"], Some(install_dir))?;
            (self.run_cmd)("git", &["checkout", r], Some(install_dir))?;
            return Ok(());
        }
        let upstream = (self.run_capture)(
            "git",
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
            Some(install_dir),
        )
        .unwrap_or_else(|_| "origin/main".to_owned());
        let branch = upstream.strip_prefix("origin/").unwrap_or("main");
        let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        (self.run_cmd)(
            "git",
            &["fetch", "--prune", "--no-tags", "origin", &refspec],
            Some(install_dir),
        )?;
        let local_head = (self.run_capture)("git", &["rev-parse", "HEAD"], Some(install_dir))?;
        let remote_head = (self.run_capture)("git", &["rev-parse", "@{upstream}"], Some(install_dir))?;
        if local_head != remote_head {
            (self.run_cmd)("git", &["reset", "--hard", "@{upstream}"], Some(install_dir))?;
            (self.run_cmd)("git", &["clean", "-fdx"], Some(install_dir))?;
        }
        Ok(())
    }

    fn update(&self, source: Option<&str>) -> Result<()> {
        let sources: Vec<String> = match source {
            Some(s) => vec![s.to_owned()],
            None => self
                .settings_manager
                .get_packages()
                .into_iter()
                .map(|p| match p {
                    PackageSource::String(s) => s,
                    PackageSource::Object(o) => o.source,
                })
                .collect(),
        };
        for src in sources {
            let parsed = self.parse_source(&src)?;
            let dir = self.get_git_install_path(&parsed, "user");
            self.refresh_git_dir(&dir, parsed.ref_name.as_deref())?;
        }
        Ok(())
    }

    fn resolve_extension_sources_temporary(&self, sources: &[&str]) -> Result<()> {
        for src in sources {
            let parsed = self.parse_source(src)?;
            let dir = self.get_git_install_path(&parsed, "temporary");
            self.refresh_git_dir(&dir, parsed.ref_name.as_deref())?;
        }
        Ok(())
    }
}

struct GitUpdateFixture {
    _net_guard: pi_js::env::testing::EnvGuard,
    _temp: TempDir,
    temp_dir: String,
    remote_dir: String,
    agent_dir: String,
    installed_dir: String,
    settings_manager: SettingsManager,
    package_manager: TestPackageManager,
}

impl GitUpdateFixture {
    fn new() -> Self {
        let net_guard = allow_network();
        let temp = TempDir::new().expect("create temp dir");
        let temp_dir = temp.path().to_string_lossy().into_owned();
        let remote_dir = pi_js::path::join(&[&temp_dir, "remote"]);
        let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
        let installed_dir = pi_js::path::join(&[&agent_dir, "git", "github.com", "test", "extension"]);
        pi_js::fs::mkdir(&agent_dir, true, None).expect("mkdir agent_dir");
        let settings_manager = SettingsManager::in_memory(None, None);
        let package_manager = TestPackageManager::new(&temp_dir, &agent_dir, settings_manager.clone());
        Self {
            _net_guard: net_guard,
            _temp: temp,
            temp_dir,
            remote_dir,
            agent_dir,
            installed_dir,
            settings_manager,
            package_manager,
        }
    }

    fn setup_remote_and_install(&self, git_source: &str) {
        pi_js::fs::mkdir(&self.remote_dir, true, None).expect("mkdir remote_dir");
        init_git_repo(&self.remote_dir);
        create_commit(&self.remote_dir, "extension.ts", "// v1", "Initial commit");

        let parent = pi_js::path::join(&[&self.agent_dir, "git", "github.com", "test"]);
        pi_js::fs::mkdir(&parent, true, None).expect("mkdir parent");
        git(&["clone", &self.remote_dir, &self.installed_dir], &self.temp_dir);
        git(
            &["config", "--local", "user.email", "test@test.com"],
            &self.installed_dir,
        );
        git(&["config", "--local", "user.name", "Test"], &self.installed_dir);

        self.settings_manager
            .set_packages(vec![PackageSource::String(git_source.to_owned())]);
    }
}

mod default_package_manager_git_update {
    use super::*;

    const GIT_SOURCE: &str = "git:github.com/test/extension";

    mod normal_updates_no_force_push {
        use super::*;
        use pretty_assertions::assert_eq;

        #[tokio::test(flavor = "current_thread")]
        async fn should_skip_reset_clean_and_install_when_already_up_to_date() {
            let mut fx = GitUpdateFixture::new();
            pi_js::fs::mkdir(&fx.remote_dir, true, None).unwrap();
            init_git_repo(&fx.remote_dir);
            pi_js::fs::write_file(
                &pi_js::path::join(&[&fx.remote_dir, "package.json"]),
                r#"{"name":"test-extension","version":"1.0.0"}"#,
                None,
            )
            .unwrap();
            create_commit(&fx.remote_dir, "extension.ts", "// v1", "Initial commit");

            pi_js::fs::mkdir(
                &pi_js::path::join(&[&fx.agent_dir, "git", "github.com", "test"]),
                true,
                None,
            )
            .unwrap();
            git(&["clone", &fx.remote_dir, &fx.installed_dir], &fx.temp_dir);
            fx.settings_manager
                .set_packages(vec![PackageSource::String(GIT_SOURCE.to_owned())]);

            let executed = Arc::new(Mutex::new(Vec::<String>::new()));
            let executed_clone = executed.clone();
            fx.package_manager.run_cmd = Arc::new(move |command, args, cwd| {
                executed_clone
                    .lock()
                    .unwrap()
                    .push(format!("{command} {}", args.join(" ")));
                if command == "npm" {
                    return Ok(());
                }
                let mut cmd = Command::new(command);
                cmd.args(args);
                if let Some(dir) = cwd {
                    cmd.current_dir(dir);
                }
                let out = cmd.output().map_err(|e| Error::msg(e.to_string()))?;
                if !out.status.success() {
                    return Err(Error::msg(String::from_utf8_lossy(&out.stderr).into_owned()));
                }
                Ok(())
            });

            fx.package_manager.update(None).unwrap();

            let cmds = executed.lock().unwrap().clone();
            assert!(
                cmds.contains(
                    &"git fetch --prune --no-tags origin +refs/heads/main:refs/remotes/origin/main".to_owned()
                )
            );
            assert!(!cmds.contains(&"git fetch --prune origin".to_owned()));
            assert!(!cmds.contains(&"git reset --hard @{upstream}".to_owned()));
            assert!(!cmds.contains(&"git reset --hard origin/HEAD".to_owned()));
            assert!(!cmds.contains(&"git clean -fdx".to_owned()));
            assert!(!cmds.contains(&"npm install".to_owned()));
        }

        #[tokio::test(flavor = "current_thread")]
        async fn should_update_to_latest_commit_when_remote_has_new_commits() {
            let fx = GitUpdateFixture::new();
            fx.setup_remote_and_install(GIT_SOURCE);
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// v1");

            let new_commit = create_commit(&fx.remote_dir, "extension.ts", "// v2", "Second commit");
            fx.package_manager.update(None).unwrap();

            assert_eq!(get_current_commit(&fx.installed_dir), new_commit);
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// v2");
        }
    }

    mod force_push_scenarios {
        use super::*;
        use pretty_assertions::assert_eq;

        #[tokio::test(flavor = "current_thread")]
        async fn should_handle_complete_history_rewrite() {
            let fx = GitUpdateFixture::new();
            fx.setup_remote_and_install(GIT_SOURCE);

            create_commit(&fx.remote_dir, "extension.ts", "// v2", "v2");
            create_commit(&fx.remote_dir, "extension.ts", "// v3", "v3");
            fx.package_manager.update(None).unwrap();
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// v3");

            git(&["reset", "--hard", "HEAD~2"], &fx.remote_dir);
            create_commit(&fx.remote_dir, "extension.ts", "// rewrite-a", "Rewrite A");
            let final_commit = create_commit(&fx.remote_dir, "extension.ts", "// rewrite-b", "Rewrite B");

            fx.package_manager.update(None).unwrap();

            assert_eq!(get_current_commit(&fx.installed_dir), final_commit);
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// rewrite-b");
        }
    }

    mod pinned_sources {
        use super::*;
        use pretty_assertions::assert_eq;

        #[tokio::test(flavor = "current_thread")]
        async fn should_checkout_the_configured_pinned_git_ref_during_full_and_targeted_updates() {
            let fx = GitUpdateFixture::new();
            pi_js::fs::mkdir(&fx.remote_dir, true, None).unwrap();
            init_git_repo(&fx.remote_dir);
            let v1_commit = create_commit(&fx.remote_dir, "extension.ts", "// v1", "Initial commit");
            git(&["tag", "v1"], &fx.remote_dir);
            let v2_commit = create_commit(&fx.remote_dir, "extension.ts", "// v2", "Second commit");
            git(&["tag", "v2"], &fx.remote_dir);

            pi_js::fs::mkdir(
                &pi_js::path::join(&[&fx.agent_dir, "git", "github.com", "test"]),
                true,
                None,
            )
            .unwrap();
            git(&["clone", &fx.remote_dir, &fx.installed_dir], &fx.temp_dir);
            git(&["checkout", "v1"], &fx.installed_dir);
            assert_eq!(get_current_commit(&fx.installed_dir), v1_commit);

            let pinned_source = format!("{GIT_SOURCE}@v2");
            fx.settings_manager
                .set_packages(vec![PackageSource::String(pinned_source.clone())]);

            fx.package_manager.update(None).unwrap();
            assert_eq!(get_current_commit(&fx.installed_dir), v2_commit);
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// v2");

            git(&["checkout", "v1"], &fx.installed_dir);
            fx.package_manager.update(Some(&pinned_source)).unwrap();
            assert_eq!(get_current_commit(&fx.installed_dir), v2_commit);
            assert_eq!(get_file_content(&fx.installed_dir, "extension.ts"), "// v2");
        }
    }

    mod temporary_git_sources {
        use super::*;
        use pretty_assertions::assert_eq;

        #[tokio::test(flavor = "current_thread")]
        async fn should_refresh_cached_temporary_git_sources_when_resolving() {
            let mut fx = GitUpdateFixture::new();
            let parsed = fx.package_manager.parse_source(GIT_SOURCE).unwrap();
            let cached_dir = fx.package_manager.get_git_install_path(&parsed, "temporary");
            let extension_file = pi_js::path::join(&[&cached_dir, "pi-extensions", "session-breakdown.ts"]);

            let _ = pi_js::fs::rm(&cached_dir, true, true);
            pi_js::fs::mkdir(&pi_js::path::join(&[&cached_dir, "pi-extensions"]), true, None).unwrap();
            pi_js::fs::write_file(
                &pi_js::path::join(&[&cached_dir, "package.json"]),
                "{\n  \"pi\": {\n    \"extensions\": [\n      \"./pi-extensions\"\n    ]\n  }\n}",
                None,
            )
            .unwrap();
            pi_js::fs::write_file(&extension_file, "// stale", None).unwrap();

            let executed = Arc::new(Mutex::new(Vec::<String>::new()));
            let executed_clone = executed.clone();
            let extension_file_clone = extension_file.clone();
            fx.package_manager.run_cmd = Arc::new(move |command, args, _cwd| {
                executed_clone
                    .lock()
                    .unwrap()
                    .push(format!("{command} {}", args.join(" ")));
                if command == "git" && args.first().copied() == Some("reset") {
                    pi_js::fs::write_file(&extension_file_clone, "// fresh", None)?;
                }
                Ok(())
            });
            fx.package_manager.run_capture = Arc::new(|_command, args, _cwd| {
                if args.first().copied() == Some("rev-parse") && args.get(1).copied() == Some("HEAD") {
                    return Ok("local-head".to_owned());
                }
                if args.first().copied() == Some("rev-parse") && args.get(1).copied() == Some("@{upstream}") {
                    return Ok("remote-head".to_owned());
                }
                if args.first().copied() == Some("rev-parse") && args.get(1).copied() == Some("--abbrev-ref") {
                    return Ok("origin/main".to_owned());
                }
                Ok(String::new())
            });

            fx.package_manager
                .resolve_extension_sources_temporary(&[GIT_SOURCE])
                .unwrap();

            let cmds = executed.lock().unwrap().clone();
            assert!(
                cmds.contains(
                    &"git fetch --prune --no-tags origin +refs/heads/main:refs/remotes/origin/main".to_owned()
                )
            );
            assert_eq!(
                get_file_content(&cached_dir, "pi-extensions/session-breakdown.ts"),
                "// fresh"
            );
        }
    }
}

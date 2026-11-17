//! Port of packages/coding-agent/test/package-manager-ssh.test.ts

use std::fs;

use pi_coding_agent::core::package_manager::{DefaultPackageManager, PackageManagerOptions, ParsedSource};
use pi_coding_agent::core::settings_manager::SettingsManager;
use tempfile::TempDir;

struct TestFixture {
    _temp_dir: TempDir,
    package_manager: DefaultPackageManager,
}

impl TestFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let agent_dir = temp_dir.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("create agent dir");

        let settings_manager = SettingsManager::in_memory(None, None);
        let package_manager = DefaultPackageManager::new(PackageManagerOptions {
            cwd: temp_dir.path().to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            settings_manager,
            builtin_extensions: Vec::new(),
        });

        Self {
            _temp_dir: temp_dir,
            package_manager,
        }
    }
}

#[test]
fn parses_https_url_without_git_prefix() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("https://github.com/user/repo") {
        ParsedSource::Git(git) => {
            assert_eq!(git.r#type, "git");
            assert_eq!(git.host, "github.com");
            assert_eq!(git.path, "user/repo");
        }
        other => panic!("expected git source, got {other:?}"),
    }
}

#[test]
fn parses_ssh_url_without_git_prefix() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("ssh://git@github.com/user/repo") {
        ParsedSource::Git(git) => {
            assert_eq!(git.r#type, "git");
            assert_eq!(git.host, "github.com");
            assert_eq!(git.path, "user/repo");
            assert_eq!(git.repo, "ssh://git@github.com/user/repo");
        }
        other => panic!("expected git source, got {other:?}"),
    }
}

#[test]
fn parses_shorthand_git_at_host_path_with_git_prefix() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("git:git@github.com:user/repo") {
        ParsedSource::Git(git) => {
            assert_eq!(git.r#type, "git");
            assert_eq!(git.host, "github.com");
            assert_eq!(git.path, "user/repo");
            assert_eq!(git.repo, "git@github.com:user/repo");
            assert!(!git.pinned);
        }
        other => panic!("expected git source, got {other:?}"),
    }
}

#[test]
fn parses_shorthand_host_path_with_git_prefix() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("git:github.com/user/repo") {
        ParsedSource::Git(git) => {
            assert_eq!(git.r#type, "git");
            assert_eq!(git.host, "github.com");
            assert_eq!(git.path, "user/repo");
        }
        other => panic!("expected git source, got {other:?}"),
    }
}

#[test]
fn parses_shorthand_with_ref_as_pinned() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("git:git@github.com:user/repo@v1.0.0") {
        ParsedSource::Git(git) => {
            assert_eq!(git.r#type, "git");
            assert_eq!(git.ref_name.as_deref(), Some("v1.0.0"));
            assert!(git.pinned);
        }
        other => panic!("expected git source, got {other:?}"),
    }
}

#[test]
fn treats_git_at_host_without_git_prefix_as_local() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("git@github.com:user/repo") {
        ParsedSource::Local(local) => {
            assert_eq!(local.path, "git@github.com:user/repo");
        }
        other => panic!("expected local source, got {other:?}"),
    }
}

#[test]
fn treats_host_path_without_git_prefix_as_local() {
    let fx = TestFixture::new();
    match fx.package_manager.parse_source("github.com/user/repo") {
        ParsedSource::Local(local) => {
            assert_eq!(local.path, "github.com/user/repo");
        }
        other => panic!("expected local source, got {other:?}"),
    }
}

#[test]
fn normalizes_protocol_and_shorthand_prefixed_urls_to_same_identity() {
    let fx = TestFixture::new();
    let prefixed = fx
        .package_manager
        .get_package_identity("git:git@github.com:user/repo", None)
        .expect("prefixed identity");
    let https = fx
        .package_manager
        .get_package_identity("https://github.com/user/repo", None)
        .expect("https identity");
    let ssh = fx
        .package_manager
        .get_package_identity("ssh://git@github.com/user/repo", None)
        .expect("ssh identity");

    assert_eq!(prefixed, "git:github.com/user/repo");
    assert_eq!(prefixed, https);
    assert_eq!(prefixed, ssh);
}

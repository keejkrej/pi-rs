//! Port of packages/coding-agent/test/footer-data-provider.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use pi_coding_agent::core::footer_data_provider::{FooterDataProvider, ReadonlyFooterDataProvider, find_git_paths};
use pi_js::fs::{mkdir, write_file as js_write_file};
use pi_js::path::join;
use tempfile::TempDir;

fn git_supports_reftable() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let Ok(tmp) = TempDir::new() else {
            return false;
        };
        let out = std::process::Command::new("git")
            .args(["init", "-b", "main", "--ref-format=reftable"])
            .arg(tmp.path())
            .output();
        out.is_ok_and(|o| o.status.success())
    })
}

fn create_plain_repo(base: &str) -> String {
    let repo_dir = join(&[base, "repo"]);
    let dot_git = join(&[&repo_dir, ".git"]);
    mkdir(&dot_git, true, None).unwrap();
    js_write_file(&join(&[&dot_git, "HEAD"]), "ref: refs/heads/main\n", None).unwrap();
    repo_dir
}

fn create_plain_reftable_repo(base: &str) -> String {
    let repo_dir = join(&[base, "repo"]);
    let reftable_dir = join(&[&repo_dir, ".git", "reftable"]);
    mkdir(&reftable_dir, true, None).unwrap();
    js_write_file(&join(&[&repo_dir, ".git", "HEAD"]), "ref: refs/heads/.invalid\n", None).unwrap();
    repo_dir
}

#[tokio::test]
async fn uses_head_directly_in_regular_repo_from_nested_directory() {
    let dir = TempDir::new().unwrap();
    let repo_dir = create_plain_repo(dir.path().to_str().unwrap());
    let nested_dir = join(&[&repo_dir, "src", "nested"]);
    mkdir(&nested_dir, true, None).unwrap();

    let paths = find_git_paths(&nested_dir).unwrap();
    assert_eq!(paths.repo_dir, repo_dir);

    let provider = FooterDataProvider::new(&nested_dir);
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    provider.dispose();
}

#[tokio::test]
async fn treats_unresolved_invalid_reftable_head_as_detached() {
    let dir = TempDir::new().unwrap();
    let repo_dir = create_plain_reftable_repo(dir.path().to_str().unwrap());

    let provider = FooterDataProvider::new(&repo_dir);
    assert_eq!(provider.get_git_branch().as_deref(), Some("detached"));
    provider.dispose();
}

#[tokio::test]
async fn resolves_and_updates_reftable_repo_and_worktree_branches_when_git_supports_reftable() {
    if !git_supports_reftable() {
        return;
    }

    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("reftable-repo");
    let repo_str = repo_path.to_str().unwrap();

    let status = std::process::Command::new("git")
        .args(["init", "-b", "main", "--ref-format=reftable", repo_str])
        .status()
        .unwrap();
    assert!(status.success());

    // Initial commit is needed to create a linked worktree and switch branches
    let cfg1 = std::process::Command::new("git")
        .current_dir(repo_str)
        .args(["config", "user.email", "test@example.com"])
        .status()
        .unwrap();
    assert!(cfg1.success());
    let cfg2 = std::process::Command::new("git")
        .current_dir(repo_str)
        .args(["config", "user.name", "Test User"])
        .status()
        .unwrap();
    assert!(cfg2.success());
    let commit = std::process::Command::new("git")
        .current_dir(repo_str)
        .args(["commit", "--allow-empty", "-m", "init"])
        .status()
        .unwrap();
    assert!(commit.success());

    let provider = FooterDataProvider::new(repo_str);
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));

    let changes = Arc::new(AtomicUsize::new(0));
    let changes_clone = changes.clone();
    let _unsub = provider.on_branch_change(Arc::new(move || {
        changes_clone.fetch_add(1, Ordering::SeqCst);
    }));

    // Switch branch in reftable repo and wait for debounced watcher refresh
    let switch = std::process::Command::new("git")
        .current_dir(repo_str)
        .args(["checkout", "-b", "foo"])
        .status()
        .unwrap();
    assert!(switch.success());

    let mut observed = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if provider.get_git_branch().as_deref() == Some("foo") {
            observed = provider.get_git_branch();
            break;
        }
    }
    assert_eq!(observed.as_deref(), Some("foo"));
    assert!(changes.load(Ordering::SeqCst) >= 1);

    // Worktree reftable check
    let wt_path = dir.path().join("wt-branch");
    let wt_str = wt_path.to_str().unwrap();
    let wt_add = std::process::Command::new("git")
        .current_dir(repo_str)
        .args(["worktree", "add", "-b", "wt-feat", wt_str])
        .status()
        .unwrap();
    assert!(wt_add.success());

    let wt_provider = FooterDataProvider::new(wt_str);
    assert_eq!(wt_provider.get_git_branch().as_deref(), Some("wt-feat"));

    wt_provider.dispose();
    provider.dispose();
}

#[tokio::test]
async fn updates_extension_statuses_provider_count_and_cwd() {
    let dir = TempDir::new().unwrap();
    let repo_a = create_plain_repo(&join(&[dir.path().to_str().unwrap(), "a"]));
    let repo_b_base = join(&[dir.path().to_str().unwrap(), "b"]);
    let repo_b = join(&[&repo_b_base, "repo"]);
    mkdir(&join(&[&repo_b, ".git"]), true, None).unwrap();
    js_write_file(&join(&[&repo_b, ".git", "HEAD"]), "ref: refs/heads/release\n", None).unwrap();

    let provider = FooterDataProvider::new(&repo_a);
    let ro: &dyn ReadonlyFooterDataProvider = &provider;
    assert_eq!(ro.get_git_branch().as_deref(), Some("main"));
    assert_eq!(ro.get_available_provider_count(), 0);

    provider.set_available_provider_count(3);
    assert_eq!(ro.get_available_provider_count(), 3);

    provider.set_extension_status("ext", Some("ready"));
    assert_eq!(
        ro.get_extension_statuses().get("ext").map(String::as_str),
        Some("ready")
    );
    provider.clear_extension_statuses();
    assert!(ro.get_extension_statuses().is_empty());

    provider.set_cwd(&repo_b);
    assert_eq!(ro.get_git_branch().as_deref(), Some("release"));

    provider.dispose();
}

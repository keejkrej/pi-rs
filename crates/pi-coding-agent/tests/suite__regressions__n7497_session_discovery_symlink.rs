//! Port of `packages/coding-agent/test/suite/regressions/7497-session-discovery-symlink.test.ts`.

use std::path::Path;

use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::session_manager::SessionManager;
use serde_json::json;
use tempfile::TempDir;

fn write_session(dir: &Path, id: &str, project_cwd: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let header = pi_js::json::stringify(&json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-08-03T00:00:00.000Z",
        "cwd": project_cwd.to_string_lossy(),
    }));
    std::fs::write(dir.join(format!("{id}.jsonl")), format!("{header}\n")).unwrap();
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(target, link).unwrap();
}

#[tokio::test]
async fn discovers_session_through_directory_symlink_and_preserves_alias_path() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    let sessions_dir = agent_dir.join("sessions");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let _guard = pi_js::env::testing::EnvGuard::set(*ENV_AGENT_DIR, Some(agent_dir.to_str().unwrap()));

    let target_dir = tmp.path().join("linked-sessions");
    write_session(&target_dir, "linked", &project_dir);
    let alias_dir = sessions_dir.join("--linked--");
    symlink_dir(&target_dir, &alias_dir);

    let sessions = SessionManager::list_all(None, None, None).await.unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["linked"]);
    assert_eq!(sessions[0].path, alias_dir.join("linked.jsonl").to_string_lossy());
}

#[tokio::test]
async fn ignores_broken_directory_link_without_hiding_valid_sessions() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    let sessions_dir = agent_dir.join("sessions");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let _guard = pi_js::env::testing::EnvGuard::set(*ENV_AGENT_DIR, Some(agent_dir.to_str().unwrap()));

    write_session(&sessions_dir.join("--regular--"), "regular", &project_dir);
    let target_dir = tmp.path().join("removed-sessions");
    std::fs::create_dir_all(&target_dir).unwrap();
    symlink_dir(&target_dir, &sessions_dir.join("--broken--"));
    std::fs::remove_dir_all(&target_dir).unwrap();

    let sessions = SessionManager::list_all(None, None, None).await.unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["regular"]);
}

#[cfg(unix)]
#[tokio::test]
async fn ignores_symlinks_pointing_to_files() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    let sessions_dir = agent_dir.join("sessions");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let _guard = pi_js::env::testing::EnvGuard::set(*ENV_AGENT_DIR, Some(agent_dir.to_str().unwrap()));

    write_session(&sessions_dir.join("--regular--"), "regular", &project_dir);
    let target_file = tmp.path().join("not-a-directory");
    std::fs::write(&target_file, "").unwrap();
    std::os::unix::fs::symlink(&target_file, sessions_dir.join("--file--")).unwrap();

    let sessions = SessionManager::list_all(None, None, None).await.unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["regular"]);
}

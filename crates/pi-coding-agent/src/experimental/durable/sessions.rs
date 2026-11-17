//! Port of packages/coding-agent/src/experimental/durable/sessions.ts

use std::sync::LazyLock;

use pi_js::crypto::{random_uuid, sha256_hex};
use pi_js::fs::promises::{mkdir, readdir_with_file_types, realpath};
use pi_js::path::{basename, join, resolve};
use pi_js::time::now_ms;

use crate::config::get_agent_dir;
use crate::vendor::proper_lockfile::{LockOptions, LockRelease, RetryOptions, lock};

static SESSION_DIR_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\d{13}-[0-9a-f-]{36}$").unwrap_or_else(|e| panic!("invalid durable session dir regex: {e}"))
});

/// One session directory holding `session.sqlite`, locked by this process.
pub struct SessionLocation {
    pub id: String,
    pub directory: String,
    pub database: String,
    pub cwd: String,
    pub created: bool,
    release: Option<LockRelease>,
}

impl SessionLocation {
    pub async fn release(&mut self) -> pi_js::Result<()> {
        if let Some(release) = self.release.take() {
            release.release().await?;
        }
        Ok(())
    }
}

/// Shared session directory selector for `durable-sessions` and `vacation-sessions`.
pub async fn select_session_in(
    subdir: &str,
    not_found_prefix: &str,
    cwd_input: &str,
    continue_session: bool,
) -> pi_js::Result<SessionLocation> {
    let cwd = realpath(&resolve(&[cwd_input])).await?;
    let hash = sha256_hex(cwd.as_bytes());
    let short_hash = &hash[..24.min(hash.len())];
    let root = join(&[&get_agent_dir(), "experimental", subdir, short_hash]);
    mkdir(&root, true, None).await?;

    let (directory, created) = if continue_session {
        let entries = readdir_with_file_types(&root).await?;
        let mut names: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.is_directory() && SESSION_DIR_NAME_RE.is_match(&entry.name))
            .map(|entry| entry.name)
            .collect();
        names.sort();
        let newest = names
            .pop()
            .ok_or_else(|| pi_js::Error::msg(format!("{not_found_prefix} {cwd}")))?;
        (join(&[&root, &newest]), false)
    } else {
        let name = format!("{:013}-{}", now_ms().max(0), random_uuid());
        let dir = join(&[&root, &name]);
        mkdir(&dir, false, None).await?;
        (dir, true)
    };

    let release = lock(
        &directory,
        LockOptions {
            realpath: Some(false),
            retries: Some(RetryOptions {
                retries: 12,
                factor: 1.0,
                min_timeout: 1_000,
                max_timeout: 1_000,
                randomize: false,
            }),
            stale: None,
            update: None,
            lockfile_path: None,
            on_compromised: None,
        },
    )
    .await
    .map_err(|err| {
        pi_js::Error::js_with_cause(
            "Error",
            format!("Session is already open in another process: {directory}"),
            err,
        )
    })?;

    Ok(SessionLocation {
        id: basename(&directory, None),
        database: join(&[&directory, "session.sqlite"]),
        directory,
        cwd,
        created,
        release: Some(release),
    })
}

/// A new session for `cwd`, or its newest one with `continueSession`.
pub async fn select_session(cwd_input: &str, continue_session: bool) -> pi_js::Result<SessionLocation> {
    select_session_in(
        "durable-sessions",
        "No durable session exists for",
        cwd_input,
        continue_session,
    )
    .await
}

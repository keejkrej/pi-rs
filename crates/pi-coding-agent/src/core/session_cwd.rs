//! Port of packages/coding-agent/src/core/session-cwd.ts

use std::fmt;

use pi_js::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

pub trait SessionCwdSource {
    fn get_cwd(&self) -> String;
    fn get_session_file(&self) -> Option<String>;
}

impl SessionCwdSource for crate::core::session_manager::SessionManager {
    fn get_cwd(&self) -> String {
        crate::core::session_manager::SessionManager::get_cwd(self)
    }

    fn get_session_file(&self) -> Option<String> {
        crate::core::session_manager::SessionManager::get_session_file(self)
    }
}

pub fn get_missing_session_cwd_issue(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = session_manager.get_session_file()?;
    let session_cwd = session_manager.get_cwd();
    if session_cwd.is_empty() || pi_js::fs::exists(&session_cwd) {
        return None;
    }

    Some(SessionCwdIssue {
        session_file: Some(session_file),
        session_cwd,
        fallback_cwd: fallback_cwd.to_owned(),
    })
}

pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_deref()
        .map(|p| format!("\nSession file: {p}"))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    pub message: String,
    pub issue: SessionCwdIssue,
}

impl MissingSessionCwdError {
    pub fn new(issue: SessionCwdIssue) -> Self {
        let message = format_missing_session_cwd_error(&issue);
        Self { message, issue }
    }
}

impl fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MissingSessionCwdError {}

pub fn assert_session_cwd_exists(session_manager: &dyn SessionCwdSource, fallback_cwd: &str) -> pi_js::Result<()> {
    if let Some(issue) = get_missing_session_cwd_issue(session_manager, fallback_cwd) {
        return Err(Error::typed(
            "MissingSessionCwdError",
            MissingSessionCwdError::new(issue),
        ));
    }
    Ok(())
}

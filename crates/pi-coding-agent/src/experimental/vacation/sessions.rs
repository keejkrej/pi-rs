//! Port of packages/coding-agent/src/experimental/vacation/sessions.ts

pub use crate::experimental::durable::sessions::SessionLocation;
use crate::experimental::durable::sessions::select_session_in;

/// A new session for `cwd`, or its newest one with `continueSession`.
pub async fn select_session(cwd_input: &str, continue_session: bool) -> pi_js::Result<SessionLocation> {
    select_session_in(
        "vacation-sessions",
        "No vacation session exists for",
        cwd_input,
        continue_session,
    )
    .await
}

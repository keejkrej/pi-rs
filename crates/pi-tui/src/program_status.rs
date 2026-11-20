//! Program Status Protocol (OSC 7501): a program tells the terminal whether it is idle, working,
//! blocked on the user, done, or failed. Only the root record is supported.
//!
//! Spec: <https://www.superlogical.com/rex/docs/build/program-status>

use serde::{Deserialize, Serialize};

use crate::utils::replace_control_characters;

/// State reported via OSC 7501 (`state=...`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatusState {
    #[default]
    Idle,
    Working,
    Blocked,
    Done,
    Error,
    /// Removes the status instead of reporting one.
    Clear,
}

impl ProgramStatusState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Error => "error",
            Self::Clear => "clear",
        }
    }
}

/// What a blocked program waits for (`kind=...`). Omitted for other states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgramStatusKind {
    Permission,
    Question,
    Auth,
}

impl ProgramStatusKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

/// Program status payload encoded into OSC 7501.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatus {
    /// `Clear` removes the status instead of reporting one.
    pub state: ProgramStatusState,
    /// Stable program name, `[A-Za-z0-9_.+-]{1,32}`. Other values are omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// What a blocked program waits for. Omitted for other states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgramStatusKind>,
    /// One human-readable line. Control characters become spaces; longer text is cut to the spec limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Feature detection query. A supporting terminal replies with the same body.
pub const PROGRAM_STATUS_QUERY: &str = "\x1b]7501;?\x1b\\";

/// Decoded `msg` limit. Its base64 encoding stays under the 2732-byte encoded limit.
const MAX_MESSAGE_BYTES: usize = 2048;

/// The reply to [`PROGRAM_STATUS_QUERY`]. Later spec revisions may add pairs after the `?`.
pub fn is_program_status_reply(sequence: &str) -> bool {
    let Some(body) = sequence.strip_prefix("\x1b]7501;?") else {
        return false;
    };
    let rest = if let Some(stripped) = body.strip_suffix('\x07') {
        stripped
    } else if let Some(stripped) = body.strip_suffix("\x1b\\") {
        stripped
    } else {
        return false;
    };
    !rest.contains('\x07') && !rest.contains('\x1b')
}

fn is_valid_app(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 32
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Encode a status report. Terminals discard reports whose text contains control characters, so they are replaced.
pub fn format_program_status(status: &ProgramStatus) -> String {
    let mut pairs = vec![format!("state={}", status.state.as_str())];
    if let Some(app) = status.app.as_deref()
        && is_valid_app(app)
    {
        pairs.push(format!("app={app}"));
    }
    if status.state == ProgramStatusState::Blocked
        && let Some(kind) = status.kind
    {
        pairs.push(format!("kind={}", kind.as_str()));
    }
    let cleaned = replace_control_characters(status.message.as_deref().unwrap_or(""));
    let trimmed = cleaned.trim();
    let message = truncate_utf8(trimmed, MAX_MESSAGE_BYTES);
    if !message.is_empty() {
        pairs.push(format!("msg={}", pi_js::b64::encode(message.as_bytes())));
    }
    format!("\x1b]7501;{}\x1b\\", pairs.join(":"))
}

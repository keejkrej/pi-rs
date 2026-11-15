//! Port of packages/server/src/transports/unix/types.ts

use crate::types::{ConnectionCountCallback, ServerErrorCallback};

#[derive(Clone, Default)]
pub struct UnixListenerOptions {
    pub path: String,
    /// Socket filesystem permissions. Defaults to owner read/write only (`0o600`).
    pub mode: Option<u32>,
    /// Maximum framed bytes queued per connection before a slow peer is disconnected.
    pub max_pending_bytes: Option<i64>,
    pub graceful_close_timeout_ms: Option<i64>,
    /// Used to derive and validate `max_pending_bytes`. Must match the server when customized.
    pub max_frame_length: Option<i64>,
    pub on_error: Option<ServerErrorCallback>,
}

#[derive(Clone, Default)]
pub struct UnixServerOptions {
    pub server_id: String,
    pub path: String,
    /// Socket filesystem permissions. Defaults to owner read/write only (`0o600`).
    pub mode: Option<u32>,
    /// Maximum framed bytes queued per connection before a slow peer is disconnected.
    pub max_pending_bytes: Option<i64>,
    pub graceful_close_timeout_ms: Option<i64>,
    /// Used to derive and validate `max_pending_bytes`. Must match the server when customized.
    pub max_frame_length: Option<i64>,
    pub handshake_timeout_ms: Option<i64>,
    pub on_connection_count_changed: Option<ConnectionCountCallback>,
    pub on_error: Option<ServerErrorCallback>,
}

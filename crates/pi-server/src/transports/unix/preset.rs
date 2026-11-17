//! Port of packages/server/src/transports/unix/preset.ts

use std::sync::Arc;

use pi_js::Result;

use crate::server::Server;
use crate::transports::unix::listener::create_unix_listener;
use crate::transports::unix::types::{UnixListenerOptions, UnixServerOptions};
use crate::types::{ServerHost, ServerOptions, SessionMetadata};

/// Compose [`Server`] with one Unix-domain socket listener.
pub fn create_unix_server<TMetadata: SessionMetadata>(
    host: Arc<dyn ServerHost<TMetadata>>,
    options: UnixServerOptions,
) -> Result<Server<TMetadata>> {
    let listener = create_unix_listener(UnixListenerOptions {
        path: options.path,
        mode: options.mode,
        max_pending_bytes: options.max_pending_bytes,
        graceful_close_timeout_ms: options.graceful_close_timeout_ms,
        max_frame_length: options.max_frame_length,
        on_error: options.on_error.clone(),
    })?;
    Server::new(
        host,
        ServerOptions {
            listeners: vec![listener],
            server_id: options.server_id,
            max_frame_length: options.max_frame_length,
            handshake_timeout_ms: options.handshake_timeout_ms,
            on_connection_count_changed: options.on_connection_count_changed,
            on_error: options.on_error,
        },
    )
}

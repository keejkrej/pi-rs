//! Port of packages/server/src/transports/unix/address.ts

use std::sync::LazyLock;

use pi_js::{Error, Result, path::join};
use regex::Regex;

static UUID_V4_LOWER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").expect("valid UUIDv4 regex")
});

/// Derive the local Unix socket path for one logical server identity.
pub fn get_unix_socket_path(server_id: &str, server_directory: &str) -> Result<String> {
    if !UUID_V4_LOWER_RE.is_match(server_id) {
        return Err(Error::js(
            "TypeError",
            "Unix serverId must be a canonical lowercase UUIDv4",
        ));
    }
    Ok(join(&[server_directory, &format!("{server_id}.sock")]))
}

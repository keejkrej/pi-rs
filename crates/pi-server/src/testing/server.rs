//! Port of packages/server/src/testing/server.ts

use std::sync::Arc;

use pi_js::Result;

use crate::listener::ServerListener;
use crate::server::Server;
use crate::testing::host::TestServerHost;
use crate::types::{DefaultSessionMetadata, ServerErrorCallback, ServerHost, ServerOptions};

#[derive(Default)]
pub struct TestServerOptions {
    pub listeners: Vec<Arc<dyn ServerListener>>,
    pub max_frame_length: Option<i64>,
    pub handshake_timeout_ms: Option<i64>,
    pub host: Option<Arc<dyn ServerHost<DefaultSessionMetadata>>>,
    pub server_id: Option<String>,
    pub on_error: Option<ServerErrorCallback>,
}

pub struct TestServer {
    pub server: Server<DefaultSessionMetadata>,
    pub host: Arc<dyn ServerHost<DefaultSessionMetadata>>,
}

/// Create an unstarted [`Server`] with deterministic defaults for transport conformance tests.
pub fn create_test_server(options: TestServerOptions) -> Result<TestServer> {
    let host: Arc<dyn ServerHost<DefaultSessionMetadata>> = options
        .host
        .unwrap_or_else(|| TestServerHost::new() as Arc<dyn ServerHost<DefaultSessionMetadata>>);
    let server = Server::new(
        host.clone(),
        ServerOptions {
            listeners: options.listeners,
            server_id: options
                .server_id
                .unwrap_or_else(|| "00000000-0000-4000-8000-000000000001".to_string()),
            max_frame_length: options.max_frame_length,
            handshake_timeout_ms: options.handshake_timeout_ms,
            on_connection_count_changed: None,
            on_error: options.on_error,
        },
    )?;
    Ok(TestServer { server, host })
}

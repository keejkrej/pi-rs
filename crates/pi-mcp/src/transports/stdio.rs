//! Port of packages/mcp/src/transports/stdio.ts

#![allow(dead_code, unused_variables)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

use pi_js::Unsubscribe;

use crate::protocol::jsonrpc::JsonRpcMessage;
use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener, TransportEvents,
};

const DEFAULT_MAX_STDERR_BYTES: i64 = 64 * 1024;
const DEFAULT_CLOSE_TIMEOUT_MS: i64 = 2_000;
/// How long a server gets to exit on its own after stdin closes, before it is sent SIGTERM.
const STDIN_CLOSE_GRACE_MS: i64 = 500;

/// Process groups of running servers, killed if the host exits without closing them.
static LIVE_PROCESS_GROUPS: LazyLock<Mutex<IndexSet<u32>>> = LazyLock::new(|| Mutex::new(IndexSet::new()));
static EXIT_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// `process.platform !== "win32"`. A function, not a const, so tests can override the platform.
fn use_process_groups() -> bool {
    todo!("port: use_process_groups")
}

fn kill_process_tree(child: &mut tokio::process::Child, signal: &str) {
    todo!("port: kill_process_tree")
}

fn install_exit_hook() {
    todo!("port: install_exit_hook")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StdioStderr {
    #[serde(rename = "pipe")]
    Pipe,
    #[serde(rename = "inherit")]
    Inherit,
}

#[derive(Clone)]
pub struct StdioTransportOptions {
    pub command: String,
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Option<IndexMap<String, String>>,
    pub inherit_env: Option<bool>,
    pub stderr: Option<StdioStderr>,
    pub on_stderr: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub max_message_bytes: Option<i64>,
    pub max_stderr_bytes: Option<i64>,
    /// Time to wait for the server to exit after SIGTERM before sending SIGKILL. Default: 2000.
    pub close_timeout_ms: Option<i64>,
}

struct StdioTransportState {
    child: Option<tokio::process::Child>,
    stdout_buffer: Vec<u8>,
    stderr_buffer: Vec<u8>,
    started: bool,
    closed: bool,
}

struct StdioTransportInner {
    options: StdioTransportOptions,
    events: TransportEvents,
    state: Mutex<StdioTransportState>,
}

#[derive(Clone)]
pub struct StdioTransport {
    inner: Arc<StdioTransportInner>,
}

impl StdioTransport {
    pub fn new(options: StdioTransportOptions) -> Self {
        todo!("port: StdioTransport::new")
    }

    pub fn pid(&self) -> Option<u32> {
        todo!("port: StdioTransport::pid")
    }

    pub fn stderr(&self) -> String {
        todo!("port: StdioTransport::stderr")
    }

    pub fn options(&self) -> &StdioTransportOptions {
        &self.inner.options
    }

    fn handle_stdout(&self, chunk: &[u8]) {
        todo!("port: StdioTransport::handle_stdout")
    }

    fn handle_stderr(&self, chunk: &[u8]) {
        todo!("port: StdioTransport::handle_stderr")
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&self) -> pi_js::Result<()> {
        todo!("port: StdioTransport::start")
    }

    async fn send(&self, message: JsonRpcMessage) -> pi_js::Result<()> {
        todo!("port: StdioTransport::send")
    }

    async fn close(&self) -> pi_js::Result<()> {
        todo!("port: StdioTransport::close")
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Unsubscribe {
        self.inner.events.on_message(listener)
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Unsubscribe {
        self.inner.events.on_error(listener)
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Unsubscribe {
        self.inner.events.on_close(listener)
    }
}

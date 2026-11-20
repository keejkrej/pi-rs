//! Port of packages/coding-agent/src/extensions/mcp/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod cli;
pub mod cli_lazy;
pub mod config;
pub mod log;
pub mod oauth;
pub mod resources;
pub mod runtime;
pub mod runtime_lazy;
pub mod tools;
pub mod ui;
// @generated-mods end

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::Shared;
use indexmap::{IndexMap, IndexSet};

use crate::config::get_agent_dir;
use crate::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, CommandHandlerFn, ExtensionApi, ExtensionCommandContext,
    ExtensionContext, ExtensionFactory, ExtensionMode, ExtensionUIDialogOptions, McpServersChangeEvent, ModelRegistry,
    NotifyType, RegisteredCommandOptions, RegisteredMcpServer, SessionShutdownEvent, SessionStartEvent, ToolCallEvent,
    ToolCallEventResult, ToolDefinition, ToolExposure, ToolNamespace, ToolReachScope, ToolRendererResolver,
    TurnStartEvent,
};
use crate::core::tools::truncate::slice_head;
use crate::extensions::codemode::tool::CODEMODE_TOOL_NAME;
use crate::extensions::mcp::config::{
    LoadMcpConfigOptions, LoadedMcpConfig, McpExposure, McpScope, McpServerConfig, McpServerConfigPatch,
    McpServerEntry, UpdateMcpConfigOptions, get_mcp_tool_exposure, load_mcp_config, mcp_namespace,
    update_mcp_server_config,
};
use crate::extensions::mcp::log::McpServerLog;
use crate::extensions::mcp::oauth::{
    McpOAuthCredentialStore, McpSignInCancelledError, McpSignInPrompt, SignInMcpServerOptions, sign_in_mcp_server,
};
use crate::extensions::mcp::resources::{
    LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL, McpResourceServer, create_mcp_resource_tool_definitions,
};
use crate::extensions::mcp::runtime_lazy::{
    McpServerConnection, McpServerConnectionOptions, McpTransportFactory, ServerState, create_default_transport,
};
use crate::extensions::mcp::tools::{
    CreateMcpToolDefinitionOptions, McpToolCaller, READ_MCP_RESOURCE_TOOL, create_mcp_tool_definition,
    create_mcp_tool_name, create_mcp_tool_renderers,
};
use crate::extensions::mcp::ui::{McpMenu, McpSubscribeFn, McpUi, show_mcp_manager};
use crate::extensions::tool_search::tool::TOOL_SEARCH_TOOL_NAME;
use crate::utils::open_browser::open_browser;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::{BoxFuture, Error, Result, Unsubscribe};
use pi_tui::autocomplete::AutocompleteItem;
use pi_tui::components::select_list::SelectItem;

pub use crate::extensions::mcp::runtime::McpTransportFactory as McpTransportFactoryExport;

pub type McpLoadConfigFn = Arc<dyn Fn(&ExtensionContext) -> LoadedMcpConfig + Send + Sync>;
pub type McpOpenUrlFn = Arc<dyn Fn(&str) + Send + Sync>;
pub type McpUpdateConfigFn = Arc<dyn Fn(&McpServerEntry, McpServerConfigPatch) -> Result<()> + Send + Sync>;

#[derive(Clone, Default)]
pub struct McpExtensionOptions {
    /// Defaults to reading `mcp.json` from the agent directory and the trusted project.
    pub load_config: Option<McpLoadConfigFn>,
    /// Defaults to stdio and streamable HTTP transports built from the server config.
    pub create_transport: Option<McpTransportFactory>,
    /// Defaults to `mcp-auth.json` in the agent directory.
    pub credentials: Option<McpOAuthCredentialStore>,
    /// File server log messages are appended to. Defaults to `mcp.log` in the agent directory.
    pub log_path: Option<String>,
    /// Opens the OAuth authorization URL. Defaults to the platform browser.
    pub open_url: Option<McpOpenUrlFn>,
    /// Saves `/mcp` changes to the server's config file: its project `override_path` when set, else its
    /// `source`. Defaults to editing that `mcp.json`.
    pub update_config: Option<McpUpdateConfigFn>,
    /// How long the first prompt waits for servers with `direct` tools that are still connecting
    /// at startup, in milliseconds. Default: 10000.
    pub startup_wait_ms: Option<u64>,
}

const DEFAULT_STARTUP_WAIT_MS: u64 = 10_000;
const MAX_SERVER_DESCRIPTION_CHARS: usize = 250;
pub const MCP_SERVERS_SECTION: &str = "mcp_servers";
pub const MAX_SERVERS_SECTION_CHARS: usize = 4096;
const MCP_USAGE: &str = "Usage: /mcp, /mcp login [server], /mcp logout [server], /mcp reconnect [server]";

static MCP_TOOL_NAME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^mcp__(.+?)__(.+)$").expect("valid mcp tool name regex"));

fn is_resource_tool_name(name: &str) -> bool {
    matches!(
        name,
        LIST_MCP_RESOURCES_TOOL | LIST_MCP_RESOURCE_TEMPLATES_TOOL | READ_MCP_RESOURCE_TOOL
    )
}

#[derive(Clone)]
struct McpServer {
    instance_id: u64,
    entry: McpServerEntry,
    connection: Option<Arc<McpServerConnection>>,
    registered_config: Option<String>,
    message: Option<String>,
    /// Identifies the current connection attempt; disabling or replacing it invalidates earlier work.
    attempt: Option<u64>,
    /// Settles when the connection started for the server connected or failed.
    ready: Option<(u64, Shared<BoxFuture<()>>)>,
    /// Cleanup of a detached connection; a replacement waits for it before opening a transport.
    closing: Option<(u64, Shared<BoxFuture<()>>)>,
}

const EXPOSURE_DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "codemode",
        "called from codemode scripts, which find them with searchTools()",
    ),
    (
        "deferred",
        "not declared until tool_search loads them, then called directly; no codemode needed",
    ),
    ("direct", "declared to the model like built-in tools"),
];

fn exposure_description(exposure: McpExposure) -> Option<&'static str> {
    match exposure {
        McpExposure::Codemode => Some(EXPOSURE_DESCRIPTIONS[0].1),
        McpExposure::Deferred => Some(EXPOSURE_DESCRIPTIONS[1].1),
        McpExposure::Direct => Some(EXPOSURE_DESCRIPTIONS[2].1),
        McpExposure::Hidden => None,
    }
}

fn error_message(error: &Error) -> String {
    error.message().to_string()
}

fn first_line(text: &str) -> &str {
    text.split('\n').next().unwrap_or("")
}

fn config_enabled(config: &McpServerConfig) -> Option<bool> {
    match config {
        McpServerConfig::Stdio(s) => s.enabled,
        McpServerConfig::Http(h) => h.enabled,
    }
}

fn config_exposure(config: &McpServerConfig) -> Option<McpExposure> {
    match config {
        McpServerConfig::Stdio(s) => s.exposure,
        McpServerConfig::Http(h) => h.exposure,
    }
}

fn config_description(config: &McpServerConfig) -> Option<&str> {
    match config {
        McpServerConfig::Stdio(s) => s.description.as_deref(),
        McpServerConfig::Http(h) => h.description.as_deref(),
    }
}

fn config_tool_exposure(config: &McpServerConfig) -> Option<&IndexMap<String, McpExposure>> {
    match config {
        McpServerConfig::Stdio(s) => s.tool_exposure.as_ref(),
        McpServerConfig::Http(h) => h.tool_exposure.as_ref(),
    }
}

fn apply_config_patch(config: &mut McpServerConfig, patch: McpServerConfigPatch) {
    match config {
        McpServerConfig::Stdio(s) => {
            if let Some(enabled) = patch.enabled {
                s.enabled = Some(enabled);
            }
            if let Some(exposure) = patch.exposure {
                s.exposure = Some(exposure);
            }
        }
        McpServerConfig::Http(h) => {
            if let Some(enabled) = patch.enabled {
                h.enabled = Some(enabled);
            }
            if let Some(exposure) = patch.exposure {
                h.exposure = Some(exposure);
            }
        }
    }
}

fn is_enabled(entry: &McpServerEntry) -> bool {
    config_enabled(&entry.config) != Some(false)
}

fn exposure_of(entry: &McpServerEntry) -> McpExposure {
    config_exposure(&entry.config).unwrap_or(McpExposure::Codemode)
}

fn exposure_label(exposure: McpExposure) -> &'static str {
    match exposure {
        McpExposure::Codemode => "codemode",
        McpExposure::Deferred => "deferred",
        McpExposure::Direct => "direct",
        McpExposure::Hidden => "hidden",
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ExposureSet {
    codemode: bool,
    deferred: bool,
    direct: bool,
    hidden: bool,
}

impl ExposureSet {
    fn insert(&mut self, exposure: McpExposure) {
        match exposure {
            McpExposure::Codemode => self.codemode = true,
            McpExposure::Deferred => self.deferred = true,
            McpExposure::Direct => self.direct = true,
            McpExposure::Hidden => self.hidden = true,
        }
    }

    fn contains(&self, exposure: McpExposure) -> bool {
        match exposure {
            McpExposure::Codemode => self.codemode,
            McpExposure::Deferred => self.deferred,
            McpExposure::Direct => self.direct,
            McpExposure::Hidden => self.hidden,
        }
    }

    fn merge(&mut self, other: &ExposureSet) {
        self.codemode |= other.codemode;
        self.deferred |= other.deferred;
        self.direct |= other.direct;
        self.hidden |= other.hidden;
    }
}

fn configured_exposures(entry: &McpServerEntry) -> ExposureSet {
    let mut out = ExposureSet::default();
    out.insert(exposure_of(entry));
    if let Some(map) = config_tool_exposure(&entry.config) {
        for exp in map.values() {
            out.insert(*exp);
        }
    }
    out
}

fn has_direct_tools(entry: &McpServerEntry) -> bool {
    configured_exposures(entry).contains(McpExposure::Direct)
}

fn has_indirect_tools(entry: &McpServerEntry) -> bool {
    let exposures = configured_exposures(entry);
    exposures.contains(McpExposure::Codemode) || exposures.contains(McpExposure::Deferred)
}

fn servers_section_intro(reaches: &IndexSet<&str>) -> String {
    let mut intro = String::from("MCP servers whose tools are not declared to you.");
    if reaches.contains("codemode") {
        intro.push_str(" Call the tools of `codemode` servers from codemode scripts.");
    }
    if reaches.contains("tool_search") {
        intro.push_str(" Load the tools of `tool_search` servers with `tool_search`.");
    }
    intro
}

fn truncate_chars_utf16(text: &str, max: usize) -> String {
    let utf16_len: usize = text.encode_utf16().count();
    if utf16_len <= max {
        return text.to_string();
    }
    if max <= 1 {
        return String::new();
    }
    format!("{}…", pi_js::str16::trim_end(slice_head(text, max - 1)))
}

#[derive(Clone, Debug, Default)]
pub struct McpServerListingConnection {
    pub instructions: Option<String>,
}

/// What the `mcp_servers` section needs of a server.
#[derive(Clone, Debug)]
pub struct McpServerListing {
    pub entry: McpServerEntry,
    pub connection: Option<McpServerListingConnection>,
}

fn server_summary(server: &McpServerListing) -> String {
    let from_desc = config_description(&server.entry.config)
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty());
    let text = from_desc
        .or_else(|| server.connection.as_ref().and_then(|c| c.instructions.as_deref()))
        .unwrap_or("");
    pi_js::str16::trim(first_line(text)).to_string()
}

/// The `mcp_servers` section: every enabled server with codemode or deferred tools.
pub fn render_servers_section(servers: &[McpServerListing]) -> Option<String> {
    let mut listed: Vec<&McpServerListing> = servers
        .iter()
        .filter(|s| is_enabled(&s.entry) && has_indirect_tools(&s.entry))
        .collect();
    if listed.is_empty() {
        return None;
    }
    listed.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));

    let reaches: Vec<&'static str> = listed
        .iter()
        .map(|s| {
            if configured_exposures(&s.entry).contains(McpExposure::Codemode) {
                "codemode"
            } else {
                "tool_search"
            }
        })
        .collect();
    let reaches_set: IndexSet<&str> = reaches.iter().copied().collect();
    let intro = servers_section_intro(&reaches_set);
    let heads: Vec<String> = listed
        .iter()
        .enumerate()
        .map(|(i, s)| format!("- {} ({})", mcp_namespace(&s.entry.name), reaches[i]))
        .collect();

    let omitted = |count: usize| -> Vec<String> {
        if count > 0 {
            let suffix = if count == 1 { "" } else { "s" };
            vec![format!(
                "- … {count} more server{suffix}; find their tools with searchTools()"
            )]
        } else {
            Vec::new()
        }
    };

    let size = |kept: usize| -> usize {
        let mut parts = Vec::with_capacity(1 + kept + 1);
        parts.push(intro.as_str());
        for head in &heads[..kept] {
            parts.push(head.as_str());
        }
        let om = omitted(listed.len() - kept);
        for line in &om {
            parts.push(line.as_str());
        }
        parts.join("\n").encode_utf16().count()
    };

    let mut kept = listed.len();
    while kept > 0 && size(kept) > MAX_SERVERS_SECTION_CHARS {
        kept -= 1;
    }

    let per_server = if kept == 0 {
        0
    } else {
        let avail = MAX_SERVERS_SECTION_CHARS.saturating_sub(size(kept));
        let budget = (avail / kept).saturating_sub(2);
        MAX_SERVER_DESCRIPTION_CHARS.min(budget)
    };

    let mut out_lines = Vec::with_capacity(1 + kept + 1);
    out_lines.push(intro);
    for (idx, server) in listed[..kept].iter().enumerate() {
        let summary = if per_server > 0 {
            truncate_chars_utf16(&server_summary(server), per_server)
        } else {
            String::new()
        };
        if summary.is_empty() {
            out_lines.push(heads[idx].clone());
        } else {
            out_lines.push(format!("{}: {summary}", heads[idx]));
        }
    }
    out_lines.extend(omitted(listed.len() - kept));
    Some(out_lines.join("\n"))
}

/// Whether a call that reaches `reach` (see `ToolDefinition::reaches_tools`) needs the server: it
/// reaches every tool, or one of its words contains the server's namespace.
fn reaches_server(reach: &ToolReachScope, server: &str) -> bool {
    match reach {
        ToolReachScope::All => true,
        ToolReachScope::Names(words) => {
            let namespace = mcp_namespace(server);
            words.iter().any(|word| word.contains(&namespace))
        }
    }
}

fn state_as_str(state: ServerState) -> &'static str {
    match state {
        ServerState::Connecting => "connecting",
        ServerState::Connected => "connected",
        ServerState::Disconnected => "disconnected",
        ServerState::NeedsAuth => "needs-auth",
        ServerState::Failed => "failed",
        ServerState::Closed => "closed",
    }
}

fn describe_state(server: &McpServer, with_error: bool) -> String {
    if !is_enabled(&server.entry) {
        return "disabled".to_string();
    }
    let Some(ref connection) = server.connection else {
        return "starting".to_string();
    };
    match connection.state() {
        ServerState::NeedsAuth => "needs sign-in".to_string(),
        ServerState::Failed => {
            if with_error {
                let err = connection.error().unwrap_or_else(|| "unknown error".to_string());
                format!("failed: {}", first_line(&err))
            } else {
                "failed".to_string()
            }
        }
        ServerState::Connected => {
            let tools = connection.tools().len();
            let resources = if connection.has_resources() {
                " · resources"
            } else {
                ""
            };
            format!(
                "connected · {tools} tool{}{resources}",
                if tools == 1 { "" } else { "s" }
            )
        }
        ServerState::Connecting => "connecting…".to_string(),
        other => state_as_str(other).to_string(),
    }
}

fn attention_rank(server: &McpServer) -> u8 {
    if !is_enabled(&server.entry) {
        return 5;
    }
    match server.connection.as_ref().map(|c| c.state()) {
        Some(ServerState::NeedsAuth) => 0,
        Some(ServerState::Failed) => 1,
        Some(ServerState::Disconnected) => 2,
        Some(ServerState::Connected) => 4,
        _ => 3,
    }
}

fn describe_transport(entry: &McpServerEntry) -> String {
    match &entry.config {
        McpServerConfig::Http(h) => h.url.clone(),
        McpServerConfig::Stdio(s) => {
            let mut parts = vec![s.command.as_str()];
            if let Some(ref args) = s.args {
                parts.extend(args.iter().map(String::as_str));
            }
            parts.join(" ")
        }
    }
}

fn default_load_config(ctx: &ExtensionContext) -> LoadedMcpConfig {
    let cwd = ctx.cwd().unwrap_or_else(|_| pi_js::env::cwd());
    let project_trusted = ctx.is_project_trusted().unwrap_or(false);
    load_mcp_config(&LoadMcpConfigOptions {
        agent_dir: get_agent_dir(),
        cwd,
        project_trusted,
    })
}

struct ManagerInner {
    servers: Vec<McpServer>,
    configured_entries: Vec<McpServerEntry>,
    config_errors: Vec<String>,
    project_config: Option<String>,
    overridden: Vec<String>,
    auto_enable_codemode: bool,
    warned_unreachable: bool,
    pending: Option<Shared<BoxFuture<()>>>,
    waited_for_startup: bool,
    /// Lifetime of the current session: aborted on `session_shutdown`, and before the first `session_start`.
    session: AbortController,
    next_id: u64,
    background_actions: IndexMap<u64, Shared<BoxFuture<()>>>,
    session_cwd: String,
    credentials: Option<McpOAuthCredentialStore>,
    model_registry: Option<ModelRegistry>,
    server_log: Option<Arc<McpServerLog>>,
    listeners: IndexMap<u64, Arc<dyn Fn() + Send + Sync>>,
    next_listener_id: u64,
    tool_owners: IndexMap<String, String>,
    server_tools: IndexMap<String, IndexSet<String>>,
    definitions: IndexMap<String, ToolDefinition>,
    resource_tools_exposure: Option<McpExposure>,
    tokens_at_sign_in: IndexMap<usize, (Arc<McpServerConnection>, String)>,
}

struct McpManager {
    pi: ExtensionApi,
    options: McpExtensionOptions,
    open_url: McpOpenUrlFn,
    update_config: McpUpdateConfigFn,
    startup_wait_ms: u64,
    inner: Mutex<ManagerInner>,
}

impl McpManager {
    fn new(pi: ExtensionApi, options: McpExtensionOptions) -> Arc<Self> {
        let open_url: McpOpenUrlFn = options.open_url.clone().unwrap_or_else(|| {
            Arc::new(|url| {
                open_browser(url);
            })
        });
        let update_config: McpUpdateConfigFn = options.update_config.clone().unwrap_or_else(|| {
            Arc::new(|entry, patch| {
                let target = entry.override_path.as_deref().unwrap_or(&entry.source);
                update_mcp_server_config(
                    target,
                    &entry.name,
                    patch,
                    Some(UpdateMcpConfigOptions {
                        r#override: entry.override_path.is_some(),
                    }),
                )
            })
        });
        let startup_wait_ms = options.startup_wait_ms.unwrap_or(DEFAULT_STARTUP_WAIT_MS);
        let credentials = options.credentials.clone();
        let session = AbortController::new();
        session.abort(None);

        Arc::new(Self {
            pi,
            options,
            open_url,
            update_config,
            startup_wait_ms,
            inner: Mutex::new(ManagerInner {
                servers: Vec::new(),
                configured_entries: Vec::new(),
                config_errors: Vec::new(),
                project_config: None,
                overridden: Vec::new(),
                auto_enable_codemode: true,
                warned_unreachable: false,
                pending: None,
                waited_for_startup: false,
                session,
                next_id: 1,
                background_actions: IndexMap::new(),
                session_cwd: pi_js::env::cwd(),
                credentials,
                model_registry: None,
                server_log: None,
                listeners: IndexMap::new(),
                next_listener_id: 0,
                tool_owners: IndexMap::new(),
                server_tools: IndexMap::new(),
                definitions: IndexMap::new(),
                resource_tools_exposure: None,
                tokens_at_sign_in: IndexMap::new(),
            }),
        })
    }

    fn alloc_id(guard: &mut ManagerInner) -> u64 {
        let id = guard.next_id;
        guard.next_id = guard.next_id.wrapping_add(1).max(1);
        id
    }

    fn track_action<F, T>(self: &Arc<Self>, work: F) -> BoxFuture<T>
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mgr = Arc::clone(self);
        let task_id = {
            let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
            Self::alloc_id(&mut guard)
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mgr_cleanup = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let res = work.await;
            let _ = tx.send(res);
            let mut guard = mgr_cleanup.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.background_actions.shift_remove(&task_id);
        });
        let bg_fut: Shared<BoxFuture<()>> = async move {
            let _ = handle.await;
        }
        .boxed()
        .shared();
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.background_actions.insert(task_id, bg_fut.clone());
        }
        Box::pin(async move {
            let _ = bg_fut.await;
            rx.await.expect("tracked background action dropped before returning")
        })
    }

    fn track_bg<F>(self: &Arc<Self>, work: F) -> Shared<BoxFuture<()>>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mgr = Arc::clone(self);
        let task_id = {
            let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
            Self::alloc_id(&mut guard)
        };
        let mgr_cleanup = Arc::clone(self);
        let handle = tokio::spawn(async move {
            work.await;
            let mut guard = mgr_cleanup.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.background_actions.shift_remove(&task_id);
        });
        let bg_fut: Shared<BoxFuture<()>> = async move {
            let _ = handle.await;
        }
        .boxed()
        .shared();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.background_actions.insert(task_id, bg_fut.clone());
        bg_fut
    }

    async fn wait_for_servers(&self, waiting_instance_ids: &[u64], signal: Option<AbortSignal>) {
        if waiting_instance_ids.is_empty() || signal.as_ref().is_some_and(AbortSignal::aborted) {
            return;
        }
        loop {
            let ready: Vec<(u64, u64, Shared<BoxFuture<()>>)> = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .servers
                    .iter()
                    .filter(|s| waiting_instance_ids.contains(&s.instance_id) && is_enabled(&s.entry))
                    .filter_map(|s| s.ready.as_ref().map(|(rid, fut)| (s.instance_id, *rid, fut.clone())))
                    .collect()
            };
            if ready.is_empty() || signal.as_ref().is_some_and(AbortSignal::aborted) {
                return;
            }
            let futs: Vec<Shared<BoxFuture<()>>> = ready.iter().map(|(_, _, f)| f.clone()).collect();
            let all = futures::future::join_all(futs);
            if let Some(ref sig) = signal {
                tokio::select! {
                    _ = all => {}
                    _ = sig.cancelled() => return,
                }
            } else {
                all.await;
            }
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let all_settled = ready.iter().all(|(inst_id, rid, _)| {
                let Some(srv) = guard.servers.iter().find(|s| s.instance_id == *inst_id) else {
                    return true;
                };
                !is_enabled(&srv.entry) || srv.ready.as_ref().is_some_and(|(cur_id, _)| cur_id == rid)
            });
            if all_settled {
                return;
            }
        }
    }

    fn emit_change(&self) {
        let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.listeners.values().cloned().collect()
        };
        for listener in listeners {
            listener();
        }
    }

    fn subscribe(self: &Arc<Self>, listener: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let id = guard.next_listener_id;
            guard.next_listener_id = guard.next_listener_id.wrapping_add(1);
            guard.listeners.insert(id, listener);
            id
        };
        let weak = Arc::downgrade(self);
        Unsubscribe::new(move || {
            if let Some(mgr) = weak.upgrade() {
                let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.listeners.shift_remove(&id);
            }
        })
    }

    fn get_credentials(&self) -> McpOAuthCredentialStore {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .credentials
            .get_or_insert_with(|| McpOAuthCredentialStore::new(None, None))
            .clone()
    }

    fn get_server_log(&self) -> Arc<McpServerLog> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .server_log
            .get_or_insert_with(|| {
                let path = self
                    .options
                    .log_path
                    .clone()
                    .unwrap_or_else(|| pi_js::path::join(&[&get_agent_dir(), "mcp.log"]));
                Arc::new(McpServerLog::new(path))
            })
            .clone()
    }

    fn registered_servers(&self, configured: &[McpServerEntry]) -> (Vec<McpServer>, Vec<String>) {
        let registered = self.pi.get_mcp_servers().unwrap_or_default();
        let mut out = Vec::new();
        let mut overridden = Vec::new();
        for RegisteredMcpServer {
            name,
            config,
            extension_path,
        } in registered
        {
            if let Some(conf) = configured
                .iter()
                .find(|e| mcp_namespace(&e.name) == mcp_namespace(&name))
            {
                overridden.push(format!(
                    "\"{name}\" registered by {extension_path} is overridden by \"{}\" in {}",
                    conf.name, conf.source
                ));
                continue;
            }
            let registered_config = Some(pi_js::json::stringify(&config));
            out.push(McpServer {
                instance_id: 0,
                entry: McpServerEntry {
                    name,
                    config,
                    source: extension_path,
                    scope: Some(McpScope::Extension),
                    override_path: None,
                },
                connection: None,
                registered_config,
                message: None,
                attempt: None,
                ready: None,
                closing: None,
            });
        }
        (out, overridden)
    }

    fn resource_servers_connections(&self) -> Vec<Arc<McpServerConnection>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .servers
            .iter()
            .filter(|s| {
                is_enabled(&s.entry)
                    && exposure_of(&s.entry) != McpExposure::Hidden
                    && s.connection.as_ref().is_some_and(|c| c.has_resources())
            })
            .filter_map(|s| s.connection.clone())
            .collect()
    }

    fn resource_servers(&self) -> Vec<Arc<dyn McpResourceServer>> {
        self.resource_servers_connections()
            .into_iter()
            .map(|c| Arc::new(c) as Arc<dyn McpResourceServer>)
            .collect()
    }

    fn has_readable_resource_server(&self, conn: &Arc<McpServerConnection>) -> bool {
        self.resource_servers_connections().iter().any(|c| Arc::ptr_eq(c, conn))
    }

    fn sync_resource_tools(self: &Arc<Self>) {
        let next_exposure = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut exposures = ExposureSet::default();
            for s in &guard.servers {
                if is_enabled(&s.entry)
                    && exposure_of(&s.entry) != McpExposure::Hidden
                    && s.connection.as_ref().is_some_and(|c| c.has_resources())
                {
                    exposures.insert(exposure_of(&s.entry));
                }
            }
            let exposure = [McpExposure::Direct, McpExposure::Codemode, McpExposure::Deferred]
                .into_iter()
                .find(|cand| exposures.contains(*cand));
            let next = exposure.unwrap_or(McpExposure::Hidden);
            if Some(next) == guard.resource_tools_exposure
                || (guard.resource_tools_exposure.is_none() && next == McpExposure::Hidden)
            {
                return;
            }
            let was_direct = guard.resource_tools_exposure == Some(McpExposure::Direct);
            guard.resource_tools_exposure = Some(next);
            (next, was_direct)
        };

        let (next, was_direct) = next_exposure;
        let weak = Arc::downgrade(self);
        let defs = create_mcp_resource_tool_definitions(
            next,
            Arc::new(move || weak.upgrade().map(|m| m.resource_servers()).unwrap_or_default()),
        );
        let names: IndexSet<String> = defs.iter().map(|d| d.name.clone()).collect();
        for def in defs {
            let _ = self.pi.register_tool(def);
        }
        if was_direct && let Ok(active) = self.pi.get_active_tools() {
            let filtered: Vec<String> = active.into_iter().filter(|name| !names.contains(name)).collect();
            let _ = self.pi.set_active_tools(filtered);
        }
    }

    fn register_tools(self: &Arc<Self>, connection: &Arc<McpServerConnection>) {
        let server = connection.name().to_string();
        let mut to_register: Vec<ToolDefinition> = Vec::new();

        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let entry = guard
                .servers
                .iter()
                .find(|s| s.entry.name == server)
                .map(|s| s.entry.clone())
                .unwrap_or_else(|| connection.entry.clone());
            let description = config_description(&entry.config)
                .map(pi_js::str16::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let namespace = ToolNamespace {
                name: mcp_namespace(&server),
                aliases: Some(vec![server.clone()]),
                description,
                instructions: connection.instructions(),
            };

            let previous = guard.server_tools.get(&server).cloned().unwrap_or_default();
            let mut current = IndexSet::new();

            let tools = connection.tools();
            let mut unique_tool_names = IndexSet::new();
            for tool in &tools {
                unique_tool_names.insert(tool.name.as_str());
            }
            let mut plain_counts: IndexMap<String, usize> = IndexMap::new();
            for tool_name in unique_tool_names {
                let base = create_mcp_tool_name(&server, tool_name, |_| false);
                *plain_counts.entry(base).or_insert(0) += 1;
            }

            for tool in tools {
                let owner = format!("{server}\0{}", tool.name);
                let assigned = create_mcp_tool_name(&server, &tool.name, |candidate| {
                    let existing_differs = guard.tool_owners.get(candidate).is_some_and(|e| e != &owner);
                    existing_differs
                        || current.contains(candidate)
                        || plain_counts.get(candidate).copied().unwrap_or(0) > 1
                });
                guard.tool_owners.insert(assigned.clone(), owner);
                current.insert(assigned.clone());

                let weak_mgr = Arc::downgrade(self);
                let server_call = server.clone();
                let tool_call_name = tool.name.clone();
                let weak_res = Arc::downgrade(self);
                let server_res = server.clone();
                let def = create_mcp_tool_definition(CreateMcpToolDefinitionOptions {
                    server: server.clone(),
                    exposure: get_mcp_tool_exposure(&entry.config, &tool.name),
                    tool,
                    name: assigned,
                    namespace: namespace.clone(),
                    timeout_ms: connection.timeout_ms(),
                    get_client: Arc::new(move || {
                        let weak_mgr = weak_mgr.clone();
                        let server_call = server_call.clone();
                        let tool_call_name = tool_call_name.clone();
                        Box::pin(async move {
                            let mgr = weak_mgr
                                .upgrade()
                                .ok_or_else(|| Error::msg(format!("MCP server \"{server_call}\" is disabled.")))?;
                            let (enabled, client, current_config) = {
                                let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                                let Some(current) = guard.servers.iter().find(|s| s.entry.name == server_call) else {
                                    return Err(Error::msg(format!("MCP server \"{server_call}\" is disabled.")));
                                };
                                (
                                    is_enabled(&current.entry),
                                    current.connection.clone(),
                                    current.entry.config.clone(),
                                )
                            };
                            if !enabled {
                                return Err(Error::msg(format!("MCP server \"{server_call}\" is disabled.")));
                            }
                            let Some(client) = client else {
                                return Err(Error::msg(format!("MCP server \"{server_call}\" is still starting.")));
                            };
                            if get_mcp_tool_exposure(&current_config, &tool_call_name) == McpExposure::Hidden
                                || (client.state() == ServerState::Connected
                                    && !client.tools().iter().any(|offered| offered.name == tool_call_name))
                            {
                                return Err(Error::msg(format!(
                                    "MCP tool \"{server_call}/{tool_call_name}\" is no longer available."
                                )));
                            }
                            Ok(Arc::new(client) as Arc<dyn McpToolCaller>)
                        })
                    }),
                    readable_resources: Some(Arc::new(move || {
                        weak_res
                            .upgrade()
                            .is_some_and(|m| m.resource_servers_connections().iter().any(|c| c.name() == server_res))
                    })),
                });
                guard.definitions.insert(def.name.clone(), def.clone());
                to_register.push(def);
            }

            for name in previous {
                if !current.contains(&name)
                    && let Some(mut def) = guard.definitions.get(&name).cloned()
                {
                    def.exposure = Some(ToolExposure::Hidden);
                    to_register.push(def);
                }
            }
            guard.server_tools.insert(server, current);
        }

        for def in to_register {
            let _ = self.pi.register_tool(def);
        }
        self.sync_resource_tools();
    }

    fn hide_tools(self: &Arc<Self>, server: &str) {
        let mut to_hide = Vec::new();
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let prev = guard.server_tools.get(server).cloned().unwrap_or_default();
            for name in prev {
                if let Some(mut def) = guard.definitions.get(&name).cloned() {
                    def.exposure = Some(ToolExposure::Hidden);
                    to_hide.push(def);
                }
            }
            guard.server_tools.insert(server.to_string(), IndexSet::new());
        }
        for def in to_hide {
            let _ = self.pi.register_tool(def);
        }
        self.sync_resource_tools();
    }

    fn ensure_discovery_active(&self, ctx: &ExtensionContext) {
        let (needs_codemode, needs_tool_search, auto_enable_codemode) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut exposures = ExposureSet::default();
            for s in &guard.servers {
                if is_enabled(&s.entry) {
                    exposures.merge(&configured_exposures(&s.entry));
                }
            }
            (
                exposures.contains(McpExposure::Codemode),
                exposures.contains(McpExposure::Deferred),
                guard.auto_enable_codemode,
            )
        };
        if !needs_codemode && !needs_tool_search {
            return;
        }
        let Ok(tools) = self.pi.get_all_tools() else {
            return;
        };
        // Only a tool of these names that declares `reaches_tools` can reach MCP tools; another
        // extension's tool of the same name without it is never activated.
        let has_reaching_tool = |name: &str| tools.iter().any(|t| t.name == name && t.reaches_tools.is_some());
        let has_codemode = has_reaching_tool(CODEMODE_TOOL_NAME);
        let has_tool_search = has_reaching_tool(TOOL_SEARCH_TOOL_NAME);
        let Ok(active) = self.pi.get_active_tools() else {
            return;
        };
        let mut activate = Vec::new();
        if needs_codemode && has_codemode && auto_enable_codemode && !active.iter().any(|n| n == CODEMODE_TOOL_NAME) {
            activate.push(CODEMODE_TOOL_NAME.to_string());
        }
        if needs_tool_search && has_tool_search && !active.iter().any(|n| n == TOOL_SEARCH_TOOL_NAME) {
            activate.push(TOOL_SEARCH_TOOL_NAME.to_string());
        }
        let mut reachable = active.clone();
        if !activate.is_empty() {
            reachable.extend(activate);
            let _ = self.pi.set_active_tools(reachable.clone());
        }
        if (has_codemode && reachable.iter().any(|n| n == CODEMODE_TOOL_NAME))
            || (has_tool_search && reachable.iter().any(|n| n == TOOL_SEARCH_TOOL_NAME))
        {
            return;
        }
        let should_warn = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if guard.warned_unreachable {
                false
            } else {
                guard.warned_unreachable = true;
                true
            }
        };
        if should_warn && let Ok(ui) = ctx.ui() {
            let reason = if needs_codemode && has_codemode && !auto_enable_codemode {
                " (autoEnableCodemode is false)"
            } else {
                ""
            };
            ui.notify(
                &format!(
                    "MCP tools are only reachable from the codemode or tool_search tool, but neither is active{reason}; they cannot be called."
                ),
                Some(NotifyType::Warning),
            );
        }
    }

    fn stored_tokens(&self, connection: &McpServerConnection) -> String {
        let Some(url) = connection.oauth_url() else {
            return "null".to_string();
        };
        let creds = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.credentials.clone()
        };
        let Some(creds) = creds else {
            return "null".to_string();
        };
        match creds.tokens(connection.name(), url) {
            Some(tokens) => pi_js::json::stringify(&tokens),
            None => "null".to_string(),
        }
    }

    fn on_connection_change(self: &Arc<Self>, connection: &Arc<McpServerConnection>) {
        let key = Arc::as_ptr(connection) as usize;
        if connection.state() != ServerState::NeedsAuth {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.tokens_at_sign_in.shift_remove(&key);
        } else {
            let already = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.tokens_at_sign_in.contains_key(&key)
            };
            if !already {
                let snapshot = self.stored_tokens(connection);
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .tokens_at_sign_in
                    .entry(key)
                    .or_insert_with(|| (Arc::clone(connection), snapshot));
            }
        }
        self.emit_change();
    }

    async fn reconnect_signed_in(self: &Arc<Self>, ctx: &ExtensionContext) {
        let signed_in: Vec<(usize, Arc<McpServerConnection>)> = {
            let candidates: Vec<(usize, Arc<McpServerConnection>, String)> = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .tokens_at_sign_in
                    .iter()
                    .map(|(k, (c, tok))| (*k, Arc::clone(c), tok.clone()))
                    .collect()
            };
            candidates
                .into_iter()
                .filter(|(_, conn, tok)| self.stored_tokens(conn) != *tok)
                .map(|(k, conn, _)| (k, conn))
                .collect()
        };
        if signed_in.is_empty() {
            return;
        }
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for (k, _) in &signed_in {
                guard.tokens_at_sign_in.shift_remove(k);
            }
        }
        let futs = signed_in.into_iter().map(|(_, conn)| async move {
            let _ = conn.reconnect().await;
        });
        futures::future::join_all(futs).await;
        self.ensure_discovery_active(ctx);
    }

    fn is_attempt_current(&self, session_signal: &AbortSignal, server_instance_id: u64, attempt_id: u64) -> bool {
        if session_signal.aborted() {
            return false;
        }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .servers
            .iter()
            .any(|s| s.instance_id == server_instance_id && s.attempt == Some(attempt_id) && is_enabled(&s.entry))
    }

    fn create_connection(
        self: &Arc<Self>,
        session_signal: &AbortSignal,
        server_instance_id: u64,
        attempt_id: u64,
    ) -> Option<Arc<McpServerConnection>> {
        if session_signal.aborted() {
            return None;
        }
        let (entry, cwd, model_registry) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let entry = guard
                .servers
                .iter()
                .find(|s| s.instance_id == server_instance_id && s.attempt == Some(attempt_id) && is_enabled(&s.entry))
                .map(|s| s.entry.clone())?;
            (entry, guard.session_cwd.clone(), guard.model_registry.clone())
        };
        let credentials = self.get_credentials();
        let log = self.get_server_log();
        let create_transport = self
            .options
            .create_transport
            .clone()
            .unwrap_or_else(|| Arc::new(create_default_transport));

        let weak_tools = Arc::downgrade(self);
        let weak_change = Arc::downgrade(self);
        let sig_tools = session_signal.clone();
        let sig_change = session_signal.clone();
        let provider_token: Option<crate::extensions::mcp::runtime_lazy::ProviderTokenFn> =
            Some(Arc::new(move |provider: &str| {
                let reg = model_registry.clone();
                let provider = provider.to_string();
                Box::pin(async move {
                    let reg = reg?;
                    reg.get_api_key_for_provider(&provider).await
                })
            }));

        let conn = McpServerConnection::new(McpServerConnectionOptions {
            entry,
            cwd,
            create_transport,
            credentials,
            provider_token,
            on_tools: Arc::new(move |c| {
                if let Some(mgr) = weak_tools.upgrade()
                    && mgr.is_attempt_current(&sig_tools, server_instance_id, attempt_id)
                {
                    mgr.register_tools(&c);
                }
            }),
            on_change: Some(Arc::new(move |c| {
                if let Some(mgr) = weak_change.upgrade() {
                    if mgr.is_attempt_current(&sig_change, server_instance_id, attempt_id) {
                        mgr.on_connection_change(&c);
                    } else {
                        let key = Arc::as_ptr(&c) as usize;
                        let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.tokens_at_sign_in.shift_remove(&key);
                    }
                }
            })),
            log: Some(log),
        });

        let stored = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !session_signal.aborted()
                && let Some(s) = guard.servers.iter_mut().find(|s| {
                    s.instance_id == server_instance_id && s.attempt == Some(attempt_id) && is_enabled(&s.entry)
                })
            {
                s.connection = Some(Arc::clone(&conn));
                true
            } else {
                false
            }
        };
        if !stored {
            return None;
        }
        self.emit_change();
        Some(conn)
    }

    fn start_connection(
        self: &Arc<Self>,
        server_instance_id: u64,
        after: Option<Shared<BoxFuture<()>>>,
    ) -> Shared<BoxFuture<()>> {
        let (session_signal, attempt_id, ready_id) = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let attempt_id = Self::alloc_id(&mut guard);
            let ready_id = Self::alloc_id(&mut guard);
            if let Some(s) = guard.servers.iter_mut().find(|s| s.instance_id == server_instance_id) {
                s.attempt = Some(attempt_id);
            }
            (guard.session.signal(), attempt_id, ready_id)
        };
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let mgr = Arc::clone(self);
        let sig = session_signal.clone();
        let handle = tokio::spawn(async move {
            let _ = gate_rx.await;
            if let Some(after_fut) = after {
                after_fut.await;
            }
            if !mgr.is_attempt_current(&sig, server_instance_id, attempt_id) {
                return;
            }
            let Some(conn) = mgr.create_connection(&sig, server_instance_id, attempt_id) else {
                return;
            };
            if !mgr.is_attempt_current(&sig, server_instance_id, attempt_id) {
                conn.close().await;
                return;
            }
            let _ = conn.get_client().await;
        });
        let ready: Shared<BoxFuture<()>> = async move {
            let _ = handle.await;
        }
        .boxed()
        .shared();

        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = guard
                .servers
                .iter_mut()
                .find(|s| s.instance_id == server_instance_id && s.attempt == Some(attempt_id))
            {
                s.ready = Some((ready_id, ready.clone()));
            }
        }
        let _ = gate_tx.send(());
        ready
    }

    fn report_problems(&self, ctx: &ExtensionContext, only: Option<&IndexSet<String>>) {
        let lines: Vec<String> = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = if only.is_none() {
                guard.config_errors.iter().map(|e| format!("config: {e}")).collect()
            } else {
                Vec::new()
            };
            for server in &guard.servers {
                if let Some(filter) = only
                    && !filter.contains(&server.entry.name)
                {
                    continue;
                }
                if let Some(ref conn) = server.connection {
                    let st = conn.state();
                    if st == ServerState::NeedsAuth || st == ServerState::Failed {
                        out.push(format!("{}: {}", server.entry.name, describe_state(server, true)));
                    }
                }
            }
            out
        };
        if lines.is_empty() {
            return;
        }
        if let Ok(ui) = ctx.ui() {
            let body = lines
                .into_iter()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            ui.notify(
                &format!("MCP servers need attention:\n{body}\nRun /mcp to fix."),
                Some(NotifyType::Warning),
            );
        }
    }

    fn save_config(&self, server_name: &str, patch: McpServerConfigPatch, in_project: bool) -> Option<String> {
        let (entry, project_config) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let entry = guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .map(|s| s.entry.clone())?;
            (entry, guard.project_config.clone())
        };
        let mut entry = entry;
        if in_project && let Some(pc) = project_config {
            entry.override_path = Some(pc);
        }
        if entry.scope != Some(McpScope::Extension)
            && let Err(err) = (self.update_config)(&entry, patch)
        {
            let target = entry.override_path.as_deref().unwrap_or(&entry.source);
            return Some(format!("Could not update {target}: {}", error_message(&err)));
        }
        apply_config_patch(&mut entry.config, patch);
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = guard.servers.iter_mut().find(|s| s.entry.name == server_name) {
            s.entry = entry;
        }
        None
    }

    async fn sign_in(
        self: &Arc<Self>,
        server_name: &str,
        prompt: Arc<dyn McpSignInPrompt>,
        cancel: Option<AbortSignal>,
    ) -> Option<String> {
        let (connection, url, session_signal) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let conn = guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .and_then(|s| s.connection.clone());
            let url = conn.as_ref().and_then(|c| c.oauth_url().map(String::from));
            (conn, url, guard.session.signal())
        };
        let (Some(connection), Some(url)) = (connection, url) else {
            return Some(format!("MCP server \"{server_name}\" does not use OAuth."));
        };
        let settings = match connection.oauth_settings() {
            Ok(s) => s,
            Err(err) => return Some(format!("Sign-in failed: {}", error_message(&err))),
        };
        let store = match self.get_credentials().for_server(server_name, &url) {
            Ok(s) => s,
            Err(err) => return Some(format!("Sign-in failed: {}", error_message(&err))),
        };
        let signal = match cancel {
            Some(c) => AbortSignal::any(&[session_signal, c]),
            None => session_signal,
        };
        let tracked = self.track_action(sign_in_mcp_server(SignInMcpServerOptions {
            server_url: url,
            store,
            settings,
            challenge: connection.challenge(),
            prompt,
            signal: Some(signal),
        }));
        if let Err(err) = tracked.await {
            if err.downcast_ref::<McpSignInCancelledError>().is_some() || err.name() == "McpSignInCancelledError" {
                return Some("Sign-in cancelled.".to_string());
            }
            return Some(format!("Sign-in failed: {}", error_message(&err)));
        }
        connection.clear_challenge();
        if let Err(err) = connection.reconnect().await {
            return Some(format!("Signed in, but {}", error_message(&err)));
        }
        None
    }

    async fn sign_out(self: &Arc<Self>, server_name: &str) -> bool {
        let (connection, url) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let conn = guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .and_then(|s| s.connection.clone());
            let url = conn.as_ref().and_then(|c| c.oauth_url().map(String::from));
            (conn, url)
        };
        let (Some(connection), Some(url)) = (connection, url) else {
            return false;
        };
        let removed = self.get_credentials().remove(server_name, &url);
        connection.sign_out().await;
        removed
    }

    async fn reconnect(&self, server_name: &str) -> Option<String> {
        let (connection, previous, ready_id) = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let ready_id = Self::alloc_id(&mut guard);
            let srv = guard.servers.iter().find(|s| s.entry.name == server_name)?;
            (
                srv.connection.clone(),
                srv.ready.as_ref().map(|(_, r)| r.clone()),
                ready_id,
            )
        };
        let Some(connection) = connection else {
            return Some(format!("MCP server \"{server_name}\" is disabled."));
        };
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let (res_tx, res_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        let handle = tokio::spawn(async move {
            let _ = gate_rx.await;
            if let Some(prev) = previous {
                prev.await;
            }
            let res = match connection.reconnect().await {
                Ok(()) => None,
                Err(err) => Some(error_message(&err)),
            };
            let _ = res_tx.send(res);
        });
        let ready: Shared<BoxFuture<()>> = async move {
            let _ = handle.await;
        }
        .boxed()
        .shared();
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = guard.servers.iter_mut().find(|s| s.entry.name == server_name) {
                s.ready = Some((ready_id, ready.clone()));
            }
        }
        let _ = gate_tx.send(());
        ready.await;
        res_rx.await.unwrap_or(None)
    }

    async fn set_enabled(self: &Arc<Self>, server_name: &str, enabled: bool, in_project: bool) -> Option<String> {
        if let Some(failed) = self.save_config(
            server_name,
            McpServerConfigPatch {
                enabled: Some(enabled),
                exposure: None,
            },
            in_project,
        ) {
            return Some(failed);
        }
        if !enabled {
            let closing_pair: Option<(u64, Shared<BoxFuture<()>>)> = {
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let closing_id = Self::alloc_id(&mut guard);
                if let Some(srv) = guard.servers.iter_mut().find(|s| s.entry.name == server_name) {
                    srv.attempt = None;
                    let prev_conn = srv.connection.take();
                    let prev_ready = srv.ready.as_ref().map(|(_, r)| r.clone());
                    if let Some(conn) = prev_conn {
                        // A reconnect may already have detached its old client while awaiting transport
                        // shutdown. Closing the connection alone does not wait for that reconnect.
                        let closing_fut = self.track_bg(async move {
                            futures::future::join(conn.close(), async {
                                if let Some(r) = prev_ready {
                                    r.await;
                                }
                            })
                            .await;
                        });
                        srv.closing = Some((closing_id, closing_fut.clone()));
                        Some((closing_id, closing_fut))
                    } else {
                        srv.closing.clone()
                    }
                } else {
                    None
                }
            };
            self.hide_tools(server_name);
            self.emit_change();
            if let Some((closing_id, closing_fut)) = closing_pair {
                closing_fut.await;
                let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(srv) = guard.servers.iter_mut().find(|s| s.entry.name == server_name)
                    && srv.closing.as_ref().is_some_and(|(id, _)| *id == closing_id)
                {
                    srv.closing = None;
                }
            }
            return None;
        }
        let target = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .map(|s| (s.instance_id, s.closing.as_ref().map(|(_, f)| f.clone())))
        };
        if let Some((instance_id, closing)) = target {
            let ready = self.start_connection(instance_id, closing);
            ready.await;
        }
        None
    }

    fn set_exposure(self: &Arc<Self>, server_name: &str, exposure: McpExposure) -> Option<String> {
        if let Some(failed) = self.save_config(
            server_name,
            McpServerConfigPatch {
                enabled: None,
                exposure: Some(exposure),
            },
            false,
        ) {
            return Some(failed);
        }
        let connected = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .and_then(|s| s.connection.clone())
                .filter(|c| c.state() == ServerState::Connected)
        };
        if let Some(conn) = connected {
            self.register_tools(&conn);
        }
        self.sync_resource_tools();
        if let Ok(all) = self.pi.get_all_tools()
            && let Ok(active) = self.pi.get_active_tools()
        {
            let indirect: IndexSet<String> = all
                .into_iter()
                .filter(|t| t.exposure != ToolExposure::Direct)
                .map(|t| t.name)
                .collect();
            let server_tool_set = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.server_tools.get(server_name).cloned().unwrap_or_default()
            };
            let next: Vec<String> = active
                .into_iter()
                .filter(|n| !server_tool_set.contains(n) || !indirect.contains(n))
                .collect();
            let _ = self.pi.set_active_tools(next);
        }
        self.emit_change();
        None
    }

    fn servers_menu(&self) -> McpMenu {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut notices: Vec<String> = guard.config_errors.iter().map(|e| format!("config: {e}")).collect();
        for line in &guard.overridden {
            notices.push(format!("overridden: {line}"));
        }
        let mut sorted: Vec<&McpServer> = guard.servers.iter().collect();
        sorted.sort_by(|a, b| {
            attention_rank(a)
                .cmp(&attention_rank(b))
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        let items = sorted
            .into_iter()
            .map(|server| {
                let scope_text = if server.entry.override_path.is_some() {
                    "global, project override"
                } else {
                    server.entry.scope.map(|s| s.as_str()).unwrap_or(&server.entry.source)
                };
                SelectItem {
                    value: server.entry.name.clone(),
                    label: server.entry.name.clone(),
                    description: Some(format!(
                        "{} · {} · {scope_text}",
                        describe_state(server, true),
                        exposure_label(exposure_of(&server.entry))
                    )),
                }
            })
            .collect();
        let error = if notices.is_empty() {
            None
        } else {
            Some(notices.join("\n"))
        };
        let default_mcp = pi_js::path::resolve(&[&get_agent_dir(), "mcp.json"]);
        McpMenu {
            title: "MCP servers".to_string(),
            details: None,
            error,
            items,
            empty: Some(format!(
                "No MCP servers configured. Add them to {default_mcp} or .pi/mcp.json."
            )),
            selected: None,
            confirm_label: "manage".to_string(),
            cancel_label: "close".to_string(),
        }
    }

    fn server_menu(&self, name: &str) -> McpMenu {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(server) = guard.servers.iter().find(|s| s.entry.name == name) else {
            return McpMenu {
                title: name.to_string(),
                details: None,
                error: None,
                items: Vec::new(),
                empty: Some("This server is no longer configured.".to_string()),
                selected: None,
                confirm_label: String::new(),
                cancel_label: "back".to_string(),
            };
        };
        let entry = &server.entry;
        let connection = server.connection.as_ref();
        let saved = if entry.scope == Some(McpScope::Extension) {
            "for this session".to_string()
        } else if entry.override_path.is_some() {
            "saved to the project mcp.json".to_string()
        } else if let Some(scope) = entry.scope {
            format!("saved to the {} mcp.json", scope.as_str())
        } else {
            "saved to mcp.json".to_string()
        };
        // Global servers without an override can be turned on or off for the trusted project alone.
        let in_project =
            entry.scope == Some(McpScope::Global) && entry.override_path.is_none() && guard.project_config.is_some();
        let in_project_saved = "saved to the project mcp.json".to_string();
        let mut items = Vec::new();
        if !is_enabled(entry) {
            items.push(SelectItem {
                value: "enable".to_string(),
                label: "Enable".to_string(),
                description: Some(saved),
            });
            if in_project {
                items.push(SelectItem {
                    value: "enable-project".to_string(),
                    label: "Enable in this project".to_string(),
                    description: Some(in_project_saved),
                });
            }
        } else {
            let st = connection.map(|c| c.state());
            if st == Some(ServerState::NeedsAuth) {
                items.push(SelectItem {
                    value: "signin".to_string(),
                    label: "Sign in".to_string(),
                    description: Some("opens the browser".to_string()),
                });
            }
            if st == Some(ServerState::Connected)
                && let Some(c) = connection
            {
                items.push(SelectItem {
                    value: "tools".to_string(),
                    label: "Tools".to_string(),
                    description: Some(format!("{} offered", c.tools().len())),
                });
            }
            if matches!(
                st,
                Some(ServerState::Failed | ServerState::Disconnected | ServerState::Connected | ServerState::NeedsAuth)
            ) {
                items.push(SelectItem {
                    value: "reconnect".to_string(),
                    label: "Reconnect".to_string(),
                    description: None,
                });
            }
            if st == Some(ServerState::Connected) && connection.is_some_and(|c| c.oauth_url().is_some()) {
                items.push(SelectItem {
                    value: "signout".to_string(),
                    label: "Sign out".to_string(),
                    description: Some("deletes the stored credentials".to_string()),
                });
            }
            items.push(SelectItem {
                value: "exposure".to_string(),
                label: "Exposure".to_string(),
                description: Some(exposure_label(exposure_of(entry)).to_string()),
            });
            items.push(SelectItem {
                value: "disable".to_string(),
                label: "Disable".to_string(),
                description: Some(saved),
            });
            if in_project {
                items.push(SelectItem {
                    value: "disable-project".to_string(),
                    label: "Disable in this project".to_string(),
                    description: Some(in_project_saved),
                });
            }
        }
        let mut details_lines = vec![
            describe_transport(entry),
            format!(
                "{}: {}",
                entry.scope.map(|s| s.as_str()).unwrap_or("config"),
                entry.source
            ),
        ];
        if let Some(ref override_path) = entry.override_path {
            details_lines.push(format!("project override: {override_path}"));
        }
        details_lines.push(format!("State: {}", describe_state(server, false)));
        let details = details_lines.join("\n");
        let mut err_lines = Vec::new();
        if let Some(ref m) = server.message {
            err_lines.push(m.clone());
        }
        if let Some(c) = connection
            && c.state() != ServerState::Connected
            && let Some(err) = c.error()
        {
            err_lines.push(err);
        }
        let error = if err_lines.is_empty() {
            None
        } else {
            Some(err_lines.join("\n"))
        };
        let selected = items.first().map(|i| i.value.clone());
        McpMenu {
            title: format!("MCP server {name}"),
            details: Some(details),
            error,
            items,
            empty: None,
            selected,
            confirm_label: "select".to_string(),
            cancel_label: "back".to_string(),
        }
    }

    async fn show_tools(&self, ui: &Arc<dyn McpUi>, server_name: &str) {
        let menu = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(server) = guard.servers.iter().find(|s| s.entry.name == server_name) else {
                return;
            };
            let exposure = exposure_of(&server.entry);
            let overridden = config_tool_exposure(&server.entry.config).is_some_and(|m| !m.is_empty());
            let desc = exposure_description(exposure).unwrap_or("unreachable");
            let suffix = if overridden {
                "\nSome tools override it with toolExposure."
            } else {
                ""
            };
            let tools = server.connection.as_ref().map(|c| c.tools()).unwrap_or_default();
            let items = tools
                .into_iter()
                .map(|tool| {
                    let tool_exp = get_mcp_tool_exposure(&server.entry.config, &tool.name);
                    let d = first_line(tool.description.as_deref().unwrap_or(""));
                    let item_desc = if tool_exp == exposure {
                        d.to_string()
                    } else {
                        format!("[{}] {d}", exposure_label(tool_exp))
                    };
                    SelectItem {
                        value: tool.name.clone(),
                        label: tool.name,
                        description: Some(item_desc),
                    }
                })
                .collect();
            McpMenu {
                title: format!("Tools of {server_name}"),
                details: Some(format!("Exposure {}: {desc}{suffix}", exposure_label(exposure))),
                error: None,
                items,
                empty: Some("The server offers no tools.".to_string()),
                selected: None,
                confirm_label: "back".to_string(),
                cancel_label: "back".to_string(),
            }
        };
        let _ = ui.menu(Arc::new(move || menu.clone()), None).await;
    }

    async fn choose_exposure(self: &Arc<Self>, ui: &Arc<dyn McpUi>, server_name: &str) -> Option<String> {
        let (current, menu) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let server = guard.servers.iter().find(|s| s.entry.name == server_name)?;
            let current = exposure_of(&server.entry);
            let details = if server.entry.scope == Some(McpScope::Extension) {
                format!(
                    "Applies to this session; the server is registered by {}.",
                    server.entry.source
                )
            } else {
                format!(
                    "Saved to {}.",
                    server.entry.override_path.as_deref().unwrap_or(&server.entry.source)
                )
            };
            let items = EXPOSURE_DESCRIPTIONS
                .iter()
                .map(|(exp, desc)| {
                    let is_curr = *exp == exposure_label(current);
                    SelectItem {
                        value: (*exp).to_string(),
                        label: format!("{}{exp}", if is_curr { "✓ " } else { "  " }),
                        description: Some((*desc).to_string()),
                    }
                })
                .collect();
            let menu = McpMenu {
                title: format!("Exposure of {server_name}"),
                details: Some(details),
                error: None,
                items,
                empty: None,
                selected: Some(exposure_label(current).to_string()),
                confirm_label: "save".to_string(),
                cancel_label: "back".to_string(),
            };
            (current, menu)
        };
        let choice = ui.menu(Arc::new(move || menu.clone()), None).await?;
        let next = match choice.as_str() {
            "codemode" => McpExposure::Codemode,
            "deferred" => McpExposure::Deferred,
            "direct" => McpExposure::Direct,
            _ => return None,
        };
        if next == current {
            return None;
        }
        self.set_exposure(server_name, next)
    }

    async fn sign_in_with_ui(self: &Arc<Self>, ui: &Arc<dyn McpUi>, server_name: &str) -> Option<String> {
        let title = format!("Sign in to {server_name}");
        let cancel = Arc::new(AbortController::new());
        let cancel_handle = Arc::clone(&cancel);
        let on_cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            cancel_handle.abort(None);
        });
        ui.status(
            &title,
            "Contacting the authorization server…",
            Some(Arc::clone(&on_cancel)),
        );
        let prompt = Arc::new(TuiSignInPrompt {
            title,
            ui: Arc::clone(ui),
            open_url: Arc::clone(&self.open_url),
            auth_url: Mutex::new(String::new()),
            on_cancel,
        });
        self.sign_in(server_name, prompt, Some(cancel.signal())).await
    }

    fn run_in_background<F>(self: &Arc<Self>, ctx: &ExtensionContext, server_name: &str, op: F)
    where
        F: FnOnce(Arc<Self>) -> BoxFuture<Option<String>> + Send + 'static,
    {
        let (attempt, session_signal) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let attempt = guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .and_then(|s| s.attempt);
            (attempt, guard.session.signal())
        };
        let mgr = Arc::clone(self);
        let ctx = ctx.clone();
        let server_name = server_name.to_string();
        let _tracked = self.track_action(async move {
            let message = op(Arc::clone(&mgr)).await;
            if session_signal.aborted() {
                return;
            }
            let updated = {
                let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(srv) = guard.servers.iter_mut().find(|s| s.entry.name == server_name)
                    && srv.attempt == attempt
                {
                    srv.message = message;
                    true
                } else {
                    false
                }
            };
            if updated {
                mgr.ensure_discovery_active(&ctx);
                mgr.emit_change();
            }
        });
    }

    async fn run_action(
        self: &Arc<Self>,
        ui: &Arc<dyn McpUi>,
        ctx: &ExtensionContext,
        server_name: &str,
        action: &str,
    ) {
        let session_signal = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.session.signal()
        };
        let message = match action {
            "signin" => self.sign_in_with_ui(ui, server_name).await,
            "reconnect" => {
                let srv = server_name.to_string();
                // Connection state and error already report failures, including required sign-ins.
                self.run_in_background(ctx, server_name, move |mgr| {
                    Box::pin(async move {
                        let _ = mgr.reconnect(&srv).await;
                        None
                    })
                });
                None
            }
            "signout" => {
                self.sign_out(server_name).await;
                None
            }
            "tools" => {
                self.show_tools(ui, server_name).await;
                None
            }
            "exposure" => self.choose_exposure(ui, server_name).await,
            "enable" | "disable" | "enable-project" | "disable-project" => {
                let enable = action.starts_with("enable");
                let in_project = action.ends_with("-project");
                let srv = server_name.to_string();
                self.run_in_background(ctx, server_name, move |mgr| {
                    Box::pin(async move { mgr.set_enabled(&srv, enable, in_project).await })
                });
                None
            }
            _ => None,
        };
        if session_signal.aborted() {
            return;
        }
        {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(s) = guard.servers.iter_mut().find(|s| s.entry.name == server_name) {
                s.message = message;
            }
        }
        self.ensure_discovery_active(ctx);
        self.emit_change();
    }

    async fn manage(self: &Arc<Self>, ui: Arc<dyn McpUi>, ctx: ExtensionContext) {
        let sub_mgr = Arc::clone(self);
        let subscribe: McpSubscribeFn = Arc::new(move |cb| sub_mgr.subscribe(cb));
        loop {
            let list_mgr = Arc::clone(self);
            let Some(name) = ui
                .menu(Arc::new(move || list_mgr.servers_menu()), Some(Arc::clone(&subscribe)))
                .await
            else {
                return;
            };
            loop {
                let srv_mgr = Arc::clone(self);
                let srv_name = name.clone();
                let action = ui
                    .menu(
                        Arc::new(move || srv_mgr.server_menu(&srv_name)),
                        Some(Arc::clone(&subscribe)),
                    )
                    .await;
                let exists = {
                    let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    guard.servers.iter().any(|s| s.entry.name == name)
                };
                let Some(action) = action else {
                    break;
                };
                if !exists {
                    break;
                }
                self.run_action(&ui, &ctx, &name, &action).await;
            }
        }
    }

    fn format_status(&self) -> String {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.servers.is_empty() && guard.config_errors.is_empty() && guard.overridden.is_empty() {
            let default_mcp = pi_js::path::resolve(&[&get_agent_dir(), "mcp.json"]);
            return format!("No MCP servers configured. Add them to {default_mcp} or .pi/mcp.json.");
        }
        let mut lines: Vec<String> = guard
            .servers
            .iter()
            .map(|server| {
                let name = &server.entry.name;
                let exposure = exposure_label(exposure_of(&server.entry));
                let conn = server.connection.as_ref();
                if conn.is_some_and(|c| c.state() == ServerState::NeedsAuth) {
                    return format!("{name}: needs sign-in, run /mcp login {name} ({exposure})");
                }
                let tools = match conn {
                    Some(c) if c.state() == ServerState::Connected => {
                        format!(", {} tools", c.tools().len())
                    }
                    _ => String::new(),
                };
                let state = if !is_enabled(&server.entry) {
                    "disabled"
                } else if conn.is_some_and(|c| c.state() == ServerState::Disconnected) {
                    "disconnected, reconnects on next call"
                } else {
                    conn.map(|c| state_as_str(c.state())).unwrap_or("starting")
                };
                let error = match conn {
                    Some(c) if c.state() != ServerState::Connected => c
                        .error()
                        .map(|e| format!("\n    {}", e.split('\n').collect::<Vec<_>>().join("\n    ")))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                format!("{name}: {state}{tools} ({exposure}){error}")
            })
            .collect();
        for error in &guard.config_errors {
            lines.push(format!("config error: {error}"));
        }
        for line in &guard.overridden {
            lines.push(format!("overridden: {line}"));
        }
        lines.join("\n")
    }

    async fn pick_server<E, P>(
        &self,
        name: Option<&str>,
        ctx: &ExtensionCommandContext,
        eligible: E,
        preferred: P,
        none: &str,
    ) -> Option<String>
    where
        E: Fn(&McpServer) -> bool,
        P: Fn(&McpServer) -> bool,
    {
        if let Some(name) = name {
            let (found, is_elig) = {
                let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let srv = guard.servers.iter().find(|s| s.entry.name == name);
                (srv.is_some(), srv.is_some_and(&eligible))
            };
            if let Ok(ui) = ctx.base.ui() {
                if !found {
                    ui.notify(&format!("No MCP server named \"{name}\"."), Some(NotifyType::Error));
                } else if !is_elig {
                    ui.notify(none, Some(NotifyType::Error));
                }
            }
            return if found && is_elig { Some(name.to_string()) } else { None };
        }

        let (candidates, preferred_names) = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let cands: Vec<String> = guard
                .servers
                .iter()
                .filter(|s| eligible(s))
                .map(|s| s.entry.name.clone())
                .collect();
            let prefs: Vec<String> = guard
                .servers
                .iter()
                .filter(|s| eligible(s) && preferred(s))
                .map(|s| s.entry.name.clone())
                .collect();
            (cands, prefs)
        };
        if candidates.is_empty() {
            if let Ok(ui) = ctx.base.ui() {
                ui.notify(none, Some(NotifyType::Info));
            }
            return None;
        }
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
        if preferred_names.len() == 1 {
            return preferred_names.into_iter().next();
        }
        let ui = ctx.base.ui().ok()?;
        ui.select("MCP server", candidates, None).await.ok().flatten()
    }

    async fn login_command(self: &Arc<Self>, server_name: &str, ctx: &ExtensionCommandContext) {
        if !ctx.base.has_ui().unwrap_or(false) {
            if let Ok(ui) = ctx.base.ui() {
                ui.notify(
                    &format!("Signing in to MCP server \"{server_name}\" requires interactive mode."),
                    Some(NotifyType::Error),
                );
            }
            return;
        }
        let session_signal = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.session.signal()
        };
        let failure = if ctx.base.mode().ok() == Some(ExtensionMode::Tui) {
            let mgr_ui = Arc::clone(self);
            let srv = server_name.to_string();
            let slot = Arc::new(Mutex::new(None));
            let slot_cb = Arc::clone(&slot);
            let _ = show_mcp_manager(ctx, move |ui| {
                Box::pin(async move {
                    let res = mgr_ui.sign_in_with_ui(&ui, &srv).await;
                    *slot_cb.lock().unwrap_or_else(|e| e.into_inner()) = res;
                    Ok(())
                })
            })
            .await;
            slot.lock().unwrap_or_else(|e| e.into_inner()).take()
        } else {
            let prompt = Arc::new(CommandSignInPrompt {
                name: server_name.to_string(),
                ctx: ctx.clone(),
                open_url: Arc::clone(&self.open_url),
            });
            self.sign_in(server_name, prompt, None).await
        };
        if session_signal.aborted() {
            return;
        }
        if let Some(failure) = failure {
            if let Ok(ui) = ctx.base.ui() {
                let kind = if failure == "Sign-in cancelled." {
                    NotifyType::Info
                } else {
                    NotifyType::Error
                };
                ui.notify(&failure, Some(kind));
            }
            return;
        }
        self.ensure_discovery_active(&ctx.base);
        let tool_count = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .servers
                .iter()
                .find(|s| s.entry.name == server_name)
                .and_then(|s| s.connection.as_ref())
                .map(|c| c.tools().len())
                .unwrap_or(0)
        };
        if let Ok(ui) = ctx.base.ui() {
            ui.notify(
                &format!("Signed in to MCP server \"{server_name}\" ({tool_count} tools)."),
                Some(NotifyType::Info),
            );
        }
    }

    async fn wait_for_direct_servers(&self, ctx: &ExtensionContext) {
        let ready: Vec<Shared<BoxFuture<()>>> = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if guard.waited_for_startup {
                return;
            }
            guard.waited_for_startup = true;
            guard
                .servers
                .iter()
                .filter_map(|s| {
                    if is_enabled(&s.entry) && has_direct_tools(&s.entry) {
                        s.ready.as_ref().map(|(_, r)| r.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        if ready.is_empty() {
            return;
        }
        let all = futures::future::join_all(ready);
        let finished = tokio::select! {
            _ = all => true,
            _ = tokio::time::sleep(Duration::from_millis(self.startup_wait_ms)) => false,
        };
        if !finished && let Ok(ui) = ctx.ui() {
            ui.notify(
                "MCP servers are still connecting; their tools become available once connected.",
                Some(NotifyType::Info),
            );
        }
    }
}

struct TuiSignInPrompt {
    title: String,
    ui: Arc<dyn McpUi>,
    open_url: McpOpenUrlFn,
    auth_url: Mutex<String>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
}

#[async_trait]
impl McpSignInPrompt for TuiSignInPrompt {
    fn show_authorization_url(&self, url: &url::Url) {
        *self.auth_url.lock().unwrap_or_else(|e| e.into_inner()) = url.to_string();
        (self.open_url)(url.as_str());
    }

    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> Result<Option<String>> {
        let url = self.auth_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let val = self.ui.redirect_url(&self.title, &url, signal).await;
        self.ui
            .status(&self.title, "Connecting…", Some(Arc::clone(&self.on_cancel)));
        Ok(val)
    }
}

struct CommandSignInPrompt {
    name: String,
    ctx: ExtensionCommandContext,
    open_url: McpOpenUrlFn,
}

#[async_trait]
impl McpSignInPrompt for CommandSignInPrompt {
    fn show_authorization_url(&self, url: &url::Url) {
        if let Ok(ui) = self.ctx.base.ui() {
            ui.notify(
                &format!("Sign in to MCP server \"{}\" in your browser:\n{}", self.name, url),
                Some(NotifyType::Info),
            );
        }
        (self.open_url)(url.as_str());
    }

    async fn prompt_for_redirect_url(&self, signal: AbortSignal) -> Result<Option<String>> {
        let ui = self.ctx.base.ui()?;
        ui.input(
            &format!(
                "Waiting for sign-in to \"{}\". If the browser cannot reach this machine, paste the URL it was redirected to.",
                self.name
            ),
            Some("http://127.0.0.1:.../callback?code=..."),
            Some(ExtensionUIDialogOptions {
                signal: Some(signal),
                timeout: None,
            }),
        )
        .await
    }
}

fn uses_oauth(server: &McpServer) -> bool {
    server.connection.as_ref().is_some_and(|c| c.oauth_url().is_some())
}

pub fn create_mcp_extension(options: McpExtensionOptions) -> ExtensionFactory {
    Arc::new(move |pi: ExtensionApi| {
        let options = options.clone();
        Box::pin(async move {
            let mgr = McpManager::new(pi.clone(), options);

            // A resumed session renders calls to MCP tools before their server connected, if it ever does.
            let resolver: ToolRendererResolver = Arc::new(|tool_name, next| {
                next().or_else(|| {
                    let caps = MCP_TOOL_NAME_RE.captures(tool_name)?;
                    Some(create_mcp_tool_renderers(&format!("{}/{}", &caps[1], &caps[2])))
                })
            });
            let _ = pi.register_tool_renderer(resolver);

            // session_start
            let mgr_start = Arc::clone(&mgr);
            pi.on::<SessionStartEvent>(move |_event, ctx| {
                let mgr = Arc::clone(&mgr_start);
                Box::pin(async move {
                    let loaded = match &mgr.options.load_config {
                        Some(f) => f(&ctx),
                        None => default_load_config(&ctx),
                    };
                    let (reg_servers, reg_overridden) = mgr.registered_servers(&loaded.servers);
                    let (session_signal, enabled_ids) = {
                        let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.config_errors = loaded.errors;
                        guard.project_config = loaded.project_config;
                        guard.auto_enable_codemode = loaded.auto_enable_codemode.unwrap_or(true);
                        guard.warned_unreachable = false;
                        guard.waited_for_startup = false;
                        guard.session_cwd = ctx.cwd().unwrap_or_else(|_| pi_js::env::cwd());
                        guard.model_registry = ctx.model_registry().ok();
                        let session = AbortController::new();
                        let sig = session.signal();
                        guard.session = session;
                        guard.configured_entries = loaded.servers.clone();
                        guard.overridden = reg_overridden;
                        let mut next_servers = Vec::with_capacity(loaded.servers.len() + reg_servers.len());
                        for entry in loaded.servers {
                            let instance_id = McpManager::alloc_id(&mut guard);
                            next_servers.push(McpServer {
                                instance_id,
                                entry,
                                connection: None,
                                registered_config: None,
                                message: None,
                                attempt: None,
                                ready: None,
                                closing: None,
                            });
                        }
                        for mut srv in reg_servers {
                            srv.instance_id = McpManager::alloc_id(&mut guard);
                            next_servers.push(srv);
                        }
                        guard.servers = next_servers;
                        let enabled: Vec<u64> = guard
                            .servers
                            .iter()
                            .filter(|s| is_enabled(&s.entry))
                            .map(|s| s.instance_id)
                            .collect();
                        (sig, enabled)
                    };
                    mgr.emit_change();
                    mgr.ensure_discovery_active(&ctx);
                    if enabled_ids.is_empty() {
                        mgr.report_problems(&ctx, None);
                        return Ok(None);
                    }

                    // Yield one event-loop turn before connecting, matching JS setImmediate.
                    let runtime_handle = tokio::spawn(async {
                        tokio::task::yield_now().await;
                    });
                    let runtime_ready: Shared<BoxFuture<()>> = async move {
                        let _ = runtime_handle.await;
                    }
                    .boxed()
                    .shared();

                    let mut readies = Vec::with_capacity(enabled_ids.len());
                    for inst_id in enabled_ids {
                        readies.push(mgr.start_connection(inst_id, Some(runtime_ready.clone())));
                    }

                    let mgr_done = Arc::clone(&mgr);
                    let ctx_done = ctx.clone();
                    let sig_done = session_signal.clone();
                    let pending_handle = tokio::spawn(async move {
                        futures::future::join_all(readies).await;
                        if !sig_done.aborted() {
                            mgr_done.report_problems(&ctx_done, None);
                        }
                    });
                    let pending_fut: Shared<BoxFuture<()>> = async move {
                        let _ = pending_handle.await;
                    }
                    .boxed()
                    .shared();
                    {
                        let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        if !session_signal.aborted() {
                            guard.pending = Some(pending_fut);
                        }
                    }
                    Ok(None)
                })
            });

            // before_agent_start
            let mgr_before = Arc::clone(&mgr);
            pi.on::<BeforeAgentStartEvent>(move |event, ctx| {
                let mgr = Arc::clone(&mgr_before);
                Box::pin(async move {
                    mgr.wait_for_direct_servers(&ctx).await;
                    let listings: Vec<McpServerListing> = {
                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard
                            .servers
                            .iter()
                            .map(|s| McpServerListing {
                                entry: s.entry.clone(),
                                connection: s.connection.as_ref().map(|c| McpServerListingConnection {
                                    instructions: c.instructions(),
                                }),
                            })
                            .collect()
                    };
                    let section = render_servers_section(&listings);
                    if let Ok(mut opts) = event.system_prompt_options.lock() {
                        if let Some(sec) = section {
                            opts.sections.insert(MCP_SERVERS_SECTION.to_string(), sec);
                        } else {
                            opts.sections.shift_remove(MCP_SERVERS_SECTION);
                        }
                    }
                    Ok(Some(BeforeAgentStartEventResult::default()))
                })
            });

            // tool_call
            let mgr_tool_call = Arc::clone(&mgr);
            pi.on::<ToolCallEvent>(move |event, ctx| {
                let mgr = Arc::clone(&mgr_tool_call);
                Box::pin(async move {
                    let tool_name = event.tool_name().to_string();
                    let Ok(tools) = mgr.pi.get_all_tools() else {
                        return Ok(Some(ToolCallEventResult::default()));
                    };
                    let Some(tool) = tools.into_iter().find(|t| t.name == tool_name) else {
                        return Ok(Some(ToolCallEventResult::default()));
                    };
                    let ready_servers: Vec<(u64, String)> = {
                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard
                            .servers
                            .iter()
                            .filter(|s| is_enabled(&s.entry) && s.ready.is_some())
                            .map(|s| (s.instance_id, s.entry.name.clone()))
                            .collect()
                    };
                    if ready_servers.is_empty() {
                        return Ok(Some(ToolCallEventResult::default()));
                    }
                    let waiting: Vec<u64> = if let Some(ref reaches) = tool.reaches_tools {
                        let reach = reaches(&event.input_value());
                        ready_servers
                            .into_iter()
                            .filter(|(_, name)| reaches_server(&reach, name))
                            .map(|(id, _)| id)
                            .collect()
                    } else if is_resource_tool_name(&tool.name) {
                        ready_servers.into_iter().map(|(id, _)| id).collect()
                    } else {
                        let owner = {
                            let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                            guard
                                .tool_owners
                                .get(&tool.name)
                                .and_then(|o| o.split('\0').next())
                                .map(String::from)
                        };
                        match owner {
                            Some(owner_name) => ready_servers
                                .into_iter()
                                .filter(|(_, name)| name == &owner_name)
                                .map(|(id, _)| id)
                                .collect(),
                            None => Vec::new(),
                        }
                    };
                    mgr.wait_for_servers(&waiting, ctx.signal().ok().flatten()).await;
                    Ok(Some(ToolCallEventResult::default()))
                })
            });

            // turn_start
            let mgr_turn = Arc::clone(&mgr);
            pi.on::<TurnStartEvent>(move |_event, ctx| {
                let mgr = Arc::clone(&mgr_turn);
                Box::pin(async move {
                    let has_sign_in = {
                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        !guard.tokens_at_sign_in.is_empty()
                    };
                    if has_sign_in {
                        mgr.reconnect_signed_in(&ctx).await;
                    }
                    Ok(None)
                })
            });

            // mcp_servers_change
            let mgr_change = Arc::clone(&mgr);
            pi.on::<McpServersChangeEvent>(move |_event, ctx| {
                let mgr = Arc::clone(&mgr_change);
                Box::pin(async move {
                    let (session_signal, configured) = {
                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        (guard.session.signal(), guard.configured_entries.clone())
                    };
                    if session_signal.aborted() {
                        return Ok(None);
                    }
                    let (reg_servers, reg_overridden) = mgr.registered_servers(&configured);
                    let next_configs: IndexMap<String, Option<String>> = reg_servers
                        .iter()
                        .map(|s| (s.entry.name.clone(), s.registered_config.clone()))
                        .collect();

                    let (removed_names, removed_conns, added_enabled) = {
                        let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.overridden = reg_overridden;
                        let mut rem_names = IndexSet::new();
                        let mut rem_conns = Vec::new();
                        let mut kept = Vec::with_capacity(guard.servers.len());
                        for srv in guard.servers.drain(..) {
                            let should_remove = srv.entry.scope == Some(McpScope::Extension)
                                && next_configs.get(&srv.entry.name).and_then(|o| o.as_ref())
                                    != srv.registered_config.as_ref();
                            if should_remove {
                                rem_names.insert(srv.entry.name.clone());
                                if let Some(conn) = srv.connection {
                                    rem_conns.push(conn);
                                }
                            } else {
                                kept.push(srv);
                            }
                        }
                        guard.servers = kept;
                        let existing_names: IndexSet<String> =
                            guard.servers.iter().map(|s| s.entry.name.clone()).collect();
                        let mut added_entries = Vec::new();
                        for mut srv in reg_servers {
                            if !existing_names.contains(&srv.entry.name) {
                                srv.instance_id = McpManager::alloc_id(&mut guard);
                                if is_enabled(&srv.entry) {
                                    added_entries.push((srv.instance_id, srv.entry.name.clone()));
                                }
                                guard.servers.push(srv);
                            }
                        }
                        (rem_names, rem_conns, added_entries)
                    };

                    for name in &removed_names {
                        mgr.hide_tools(name);
                    }
                    mgr.emit_change();
                    mgr.ensure_discovery_active(&ctx);
                    for conn in removed_conns {
                        conn.close().await;
                    }
                    if session_signal.aborted() || added_enabled.is_empty() {
                        return Ok(None);
                    }
                    let mut readies = Vec::with_capacity(added_enabled.len());
                    for (inst_id, _) in &added_enabled {
                        readies.push(mgr.start_connection(*inst_id, None));
                    }
                    futures::future::join_all(readies).await;
                    if session_signal.aborted() {
                        let stale_ids: IndexSet<u64> = added_enabled.iter().map(|(id, _)| *id).collect();
                        let stale_conns: Vec<Arc<McpServerConnection>> = {
                            let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                            guard
                                .servers
                                .iter()
                                .filter(|s| stale_ids.contains(&s.instance_id))
                                .filter_map(|s| s.connection.clone())
                                .collect()
                        };
                        for conn in stale_conns {
                            conn.close().await;
                        }
                        return Ok(None);
                    }
                    let only: IndexSet<String> = added_enabled.into_iter().map(|(_, n)| n).collect();
                    mgr.report_problems(&ctx, Some(&only));
                    Ok(None)
                })
            });

            // session_shutdown
            let mgr_shutdown = Arc::clone(&mgr);
            pi.on::<SessionShutdownEvent>(move |_event, _ctx| {
                let mgr = Arc::clone(&mgr_shutdown);
                Box::pin(async move {
                    let (closing_conns, closing_futs, bg_futs) = {
                        let mut guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.session.abort(None);
                        let mut conns = Vec::new();
                        let mut close_futs = Vec::new();
                        for s in guard.servers.drain(..) {
                            if let Some(c) = s.connection {
                                conns.push(c);
                            }
                            if let Some((_, cf)) = s.closing {
                                close_futs.push(cf);
                            }
                        }
                        let bg: Vec<Shared<BoxFuture<()>>> = guard.background_actions.values().cloned().collect();
                        (conns, close_futs, bg)
                    };
                    mgr.emit_change();
                    futures::future::join_all(closing_conns.into_iter().map(|c| async move {
                        c.close().await;
                    }))
                    .await;
                    futures::future::join_all(closing_futs).await;
                    futures::future::join_all(bg_futs).await;
                    Ok(None)
                })
            });

            // /mcp command
            let mgr_comp = Arc::clone(&mgr);
            let mgr_cmd = Arc::clone(&mgr);
            let handler: CommandHandlerFn = Arc::new(move |args: String, ctx: ExtensionCommandContext| {
                let mgr = Arc::clone(&mgr_cmd);
                Box::pin(async move {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    let Some(&action) = parts.first() else {
                        if ctx.base.mode().ok() == Some(ExtensionMode::Tui) {
                            let mgr_ui = Arc::clone(&mgr);
                            let base_ctx = ctx.base.clone();
                            show_mcp_manager(&ctx, move |ui| {
                                Box::pin(async move {
                                    mgr_ui.manage(ui, base_ctx).await;
                                    Ok(())
                                })
                            })
                            .await?;
                        } else {
                            let pending = {
                                let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                                guard.pending.clone()
                            };
                            if let Some(p) = pending {
                                p.await;
                            }
                            if let Ok(ui) = ctx.base.ui() {
                                ui.notify(&mgr.format_status(), Some(NotifyType::Info));
                            }
                        }
                        return Ok(());
                    };
                    let name = parts.get(1).copied();
                    if parts.len() > 2 {
                        if let Ok(ui) = ctx.base.ui() {
                            ui.notify(MCP_USAGE, Some(NotifyType::Warning));
                        }
                        return Ok(());
                    }
                    let pending = {
                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.pending.clone()
                    };
                    if let Some(p) = pending {
                        p.await;
                    }
                    let oauth_none =
                        "No enabled MCP server uses OAuth. Only HTTP servers without an Authorization header do.";
                    let oauth_preferred = |s: &McpServer| {
                        s.connection
                            .as_ref()
                            .is_some_and(|c| c.state() == ServerState::NeedsAuth)
                    };
                    match action {
                        "login" => {
                            if let Some(srv) = mgr
                                .pick_server(name, &ctx, uses_oauth, oauth_preferred, oauth_none)
                                .await
                            {
                                mgr.login_command(&srv, &ctx).await;
                            }
                        }
                        "logout" => {
                            if let Some(srv) = mgr
                                .pick_server(name, &ctx, uses_oauth, oauth_preferred, oauth_none)
                                .await
                            {
                                let removed = mgr.sign_out(&srv).await;
                                if let Ok(ui) = ctx.base.ui() {
                                    let msg = if removed {
                                        format!("Signed out of MCP server \"{srv}\".")
                                    } else {
                                        format!("No stored credentials for MCP server \"{srv}\".")
                                    };
                                    ui.notify(&msg, Some(NotifyType::Info));
                                }
                            }
                        }
                        "reconnect" => {
                            let eligible = |s: &McpServer| s.connection.is_some();
                            let preferred = |s: &McpServer| {
                                s.connection.as_ref().is_some_and(|c| {
                                    matches!(c.state(), ServerState::Failed | ServerState::Disconnected)
                                })
                            };
                            if let Some(srv) = mgr
                                .pick_server(name, &ctx, eligible, preferred, "No enabled MCP server to reconnect.")
                                .await
                            {
                                let failure = mgr.reconnect(&srv).await;
                                if let Some(err) = failure {
                                    if let Ok(ui) = ctx.base.ui() {
                                        ui.notify(&err, Some(NotifyType::Error));
                                    }
                                } else {
                                    mgr.ensure_discovery_active(&ctx.base);
                                    let state_desc = {
                                        let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                                        guard
                                            .servers
                                            .iter()
                                            .find(|s| s.entry.name == srv)
                                            .map(|s| describe_state(s, true))
                                            .unwrap_or_default()
                                    };
                                    if let Ok(ui) = ctx.base.ui() {
                                        ui.notify(
                                            &format!("Reconnected to MCP server \"{srv}\" ({state_desc})."),
                                            Some(NotifyType::Info),
                                        );
                                    }
                                }
                            }
                        }
                        _ => {
                            if let Ok(ui) = ctx.base.ui() {
                                ui.notify(MCP_USAGE, Some(NotifyType::Warning));
                            }
                        }
                    }
                    Ok(())
                })
            });

            pi.register_command(
                "mcp",
                RegisteredCommandOptions {
                    description: Some(
                        "Manage MCP servers: sign in, reconnect, enable or disable, and change exposure".to_string(),
                    ),
                    get_argument_completions: Some(Arc::new(move |prefix: &str| {
                        let mgr = Arc::clone(&mgr_comp);
                        let trimmed = prefix.trim_start().to_string();
                        Box::pin(async move {
                            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                            let trailing_ws = trimmed.chars().last().is_some_and(char::is_whitespace);
                            if tokens.len() > 2 || (tokens.len() == 2 && trailing_ws) {
                                return None;
                            }
                            if tokens.is_empty() || (tokens.len() == 1 && !trailing_ws) {
                                let act = tokens.first().copied().unwrap_or("");
                                let items: Vec<AutocompleteItem> = ["login", "logout", "reconnect"]
                                    .into_iter()
                                    .filter(|item| item.starts_with(act))
                                    .map(|item| AutocompleteItem {
                                        value: format!("{item} "),
                                        label: item.to_string(),
                                        description: None,
                                    })
                                    .collect();
                                return Some(items);
                            }
                            let action = tokens[0];
                            if !matches!(action, "login" | "logout" | "reconnect") {
                                return None;
                            }
                            let server_prefix = tokens.get(1).copied().unwrap_or("");
                            let guard = mgr.inner.lock().unwrap_or_else(|e| e.into_inner());
                            let items: Vec<AutocompleteItem> = guard
                                .servers
                                .iter()
                                .filter(|c| {
                                    if action == "reconnect" {
                                        c.connection.is_some()
                                    } else {
                                        uses_oauth(c)
                                    }
                                })
                                .filter(|c| c.entry.name.starts_with(server_prefix))
                                .map(|c| AutocompleteItem {
                                    value: format!("{action} {}", c.entry.name),
                                    label: c.entry.name.clone(),
                                    description: Some(describe_state(c, true)),
                                })
                                .collect();
                            if items.is_empty() { None } else { Some(items) }
                        })
                    })),
                    handler,
                },
            )
        })
    })
}

pub fn default_export() -> ExtensionFactory {
    create_mcp_extension(McpExtensionOptions::default())
}

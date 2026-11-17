//! Port of packages/coding-agent/src/extensions/mcp/runtime.ts

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;

use crate::extensions::mcp::config::{McpServerConfig, McpServerEntry};
use crate::extensions::mcp::log::McpServerLog;
use crate::extensions::mcp::oauth::{
    McpAuthProvider, McpOAuthCredentialStore, McpOAuthSettings, create_mcp_auth_provider,
};
use crate::extensions::mcp::resources::{McpResourceServer, is_mcp_app_resource};
use crate::extensions::mcp::tools::McpToolCaller;
use crate::extensions::wire::{VERSION, resolve_config_value_or_throw, resolve_headers_or_throw};
use pi_js::{BoxFuture, Error, Result};
use pi_mcp::client::{ClientState, McpClientRoots};
use pi_mcp::oauth::{McpOAuthAuthorizationRequiredError, OAuthChallenge};
use pi_mcp::transports::stdio::StdioStderr;
use pi_mcp::transports::transport::McpTransport;
use pi_mcp::{
    AuthProvider, CallToolResult, JSON_RPC_ERROR_CODES, ListResourceTemplatesResult, ListResourcesResult,
    McpAuthRequiredError, McpClient, McpClientOptions, McpError, McpHttpError, McpRequestOptions,
    McpSessionExpiredError, ReadResourceResult, Resource, ResourceTemplate, Root, StdioTransport,
    StdioTransportOptions, StreamableHttpTransport, StreamableHttpTransportOptions, Tool as McpTool,
};

const DEFAULT_TIMEOUT_SECONDS: i64 = 60;
const STDERR_TAIL_CHARS: usize = 2_000;
const CONNECT_RETRY_DELAYS_MS: [u64; 2] = [250, 1_000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    Connecting,
    Connected,
    Disconnected,
    NeedsAuth,
    Failed,
    Closed,
}

#[derive(Clone)]
pub enum CreatedTransport {
    Stdio(Arc<StdioTransport>),
    Http(Arc<StreamableHttpTransport>),
    Custom(Arc<dyn McpTransport>),
}

impl CreatedTransport {
    pub fn as_transport(&self) -> Arc<dyn McpTransport> {
        match self {
            Self::Stdio(s) => Arc::clone(s) as Arc<dyn McpTransport>,
            Self::Http(h) => Arc::clone(h) as Arc<dyn McpTransport>,
            Self::Custom(c) => Arc::clone(c),
        }
    }

    pub fn stderr_tail(&self) -> Option<String> {
        let Self::Stdio(stdio) = self else {
            return None;
        };
        let raw = stdio.stderr();
        let trimmed = pi_js::str16::trim(&raw);
        if trimmed.is_empty() {
            return None;
        }
        let len = pi_js::str16::len(trimmed);
        if len <= STDERR_TAIL_CHARS {
            Some(trimmed.to_string())
        } else {
            Some(pi_js::str16::slice(trimmed, -(STDERR_TAIL_CHARS as i64), None))
        }
    }
}

pub type McpTransportFactory = Arc<
    dyn Fn(&McpServerEntry, &str, Option<Arc<dyn AuthProvider + Send + Sync>>) -> Result<CreatedTransport>
        + Send
        + Sync,
>;

pub fn is_transient_error(error: &Error) -> bool {
    if let Some(http_err) = error.downcast_ref::<McpHttpError>() {
        return http_err.status == 408 || http_err.status == 429 || (http_err.status >= 500 && http_err.status != 501);
    }
    error.name() == "TypeError"
}

fn sign_in_required_message(entry: &McpServerEntry) -> String {
    let target = match &entry.auth_provider {
        Some(provider) => format!("/login {provider}"),
        None => "/mcp".to_string(),
    };
    format!(
        "MCP server \"{}\" requires sign-in. Run {target} to sign in.",
        entry.name
    )
}

pub fn entry_uses_oauth(entry: &McpServerEntry) -> bool {
    let McpServerConfig::Http(ref http) = entry.config else {
        return false;
    };
    if entry.auth_provider.is_some() {
        return false;
    }
    let has_auth_header = http
        .headers
        .as_ref()
        .is_some_and(|h| h.keys().any(|k| k.eq_ignore_ascii_case("authorization")));
    !has_auth_header
}

fn expand_home(value: &str) -> String {
    if value == "~" {
        return pi_js::env::home_dir();
    }
    if value.starts_with("~/") || (pi_js::env::platform() == "win32" && value.starts_with("~\\")) {
        return pi_js::path::join(&[&pi_js::env::home_dir(), &value[2..]]);
    }
    value.to_string()
}

pub fn create_default_transport(
    entry: &McpServerEntry,
    cwd: &str,
    auth_provider: Option<Arc<dyn AuthProvider + Send + Sync>>,
) -> Result<CreatedTransport> {
    let name = &entry.name;
    match &entry.config {
        McpServerConfig::Http(http) => {
            let headers = resolve_headers_or_throw(http.headers.as_ref(), &format!("MCP server \"{name}\""), None)?;
            let transport = StreamableHttpTransport::new(StreamableHttpTransportOptions {
                url: http.url.clone(),
                headers,
                fetch: None,
                open_get_stream: None,
                max_message_bytes: None,
                auth_provider,
                reconnect: None,
            });
            Ok(CreatedTransport::Http(Arc::new(transport)))
        }
        McpServerConfig::Stdio(stdio) => {
            let mut env = IndexMap::new();
            if let Some(ref cfg_env) = stdio.env {
                for (k, v) in cfg_env {
                    env.insert(
                        k.clone(),
                        resolve_config_value_or_throw(v, &format!("MCP server \"{name}\" env \"{k}\""), None)?,
                    );
                }
            }
            let raw_cwd = expand_home(stdio.cwd.as_deref().unwrap_or("."));
            let resolved_cwd = pi_js::path::resolve(&[cwd, &raw_cwd]);
            let args = stdio
                .args
                .as_ref()
                .map(|list| list.iter().map(|a| expand_home(a)).collect());
            let transport = StdioTransport::new(StdioTransportOptions {
                command: expand_home(&stdio.command),
                args,
                cwd: Some(resolved_cwd),
                env: Some(env),
                inherit_env: None,
                stderr: Some(StdioStderr::Pipe),
                on_stderr: None,
                max_message_bytes: None,
                max_stderr_bytes: None,
                close_timeout_ms: None,
            });
            Ok(CreatedTransport::Stdio(Arc::new(transport)))
        }
    }
}

async fn without_templates<T, F, Fut>(op: F, empty: T) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    match op().await {
        Ok(val) => Ok(val),
        Err(err) => {
            if let Some(mcp_err) = err.downcast_ref::<McpError>()
                && mcp_err.code == JSON_RPC_ERROR_CODES.method_not_found
            {
                return Ok(empty);
            }
            Err(err)
        }
    }
}

async fn list_templates(client: &McpClient, options: Option<McpRequestOptions>) -> Result<Vec<ResourceTemplate>> {
    without_templates(|| client.list_resource_templates(options), Vec::new()).await
}

async fn fetch_resources(client: &McpClient) -> (Vec<Resource>, Vec<ResourceTemplate>) {
    let (res_out, tpl_out) = tokio::join!(client.list_resources(None), list_templates(client, None));
    let resources = res_out
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !is_mcp_app_resource(Some(&r.uri), r.mime_type.as_deref()))
        .collect();
    let templates = tpl_out
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !is_mcp_app_resource(Some(&t.uri_template), t.mime_type.as_deref()))
        .collect();
    (resources, templates)
}

struct ProviderTokenAuthAdapter {
    provider: String,
    lookup: Option<Arc<dyn Fn(&str) -> BoxFuture<Option<String>> + Send + Sync>>,
}

#[async_trait]
impl AuthProvider for ProviderTokenAuthAdapter {
    async fn token(&self) -> Result<Option<String>> {
        let Some(ref lookup) = self.lookup else {
            return Ok(None);
        };
        Ok(lookup(&self.provider).await)
    }

    fn on_unauthorized(&self, _context: pi_mcp::UnauthorizedContext) -> Option<BoxFuture<Result<()>>> {
        None
    }
}

#[async_trait]
impl McpAuthProvider for ProviderTokenAuthAdapter {
    async fn settled(&self) {}
}

pub type ProviderTokenFn = Arc<dyn Fn(&str) -> BoxFuture<Option<String>> + Send + Sync>;

pub struct McpServerConnectionOptions {
    pub entry: McpServerEntry,
    pub cwd: String,
    pub create_transport: McpTransportFactory,
    pub credentials: McpOAuthCredentialStore,
    pub provider_token: Option<ProviderTokenFn>,
    pub on_tools: Arc<dyn Fn(Arc<McpServerConnection>) + Send + Sync>,
    pub on_change: Option<Arc<dyn Fn(Arc<McpServerConnection>) + Send + Sync>>,
    pub log: Option<Arc<McpServerLog>>,
}

struct ConnectionMutableState {
    state: ServerState,
    error: Option<String>,
    tools: Vec<McpTool>,
    has_resources: bool,
    resources: Vec<Resource>,
    resource_templates: Vec<ResourceTemplate>,
    instructions: Option<String>,
    challenge: Option<OAuthChallenge>,
    client: Option<Arc<McpClient>>,
    opening: Option<futures::future::Shared<BoxFuture<std::result::Result<Arc<McpClient>, Arc<Error>>>>>,
    closed: bool,
    stderr_tail: Option<String>,
}

pub struct McpServerConnection {
    pub entry: McpServerEntry,
    cwd: String,
    create_transport: McpTransportFactory,
    auth_provider: Option<Arc<dyn McpAuthProvider>>,
    on_tools: Arc<dyn Fn(Arc<McpServerConnection>) + Send + Sync>,
    on_change: Option<Arc<dyn Fn(Arc<McpServerConnection>) + Send + Sync>>,
    log: Option<Arc<McpServerLog>>,
    inner: Mutex<ConnectionMutableState>,
}

fn clone_conn_err(err: &Arc<Error>) -> Error {
    if let Some(e) = err.downcast_ref::<McpOAuthAuthorizationRequiredError>() {
        return Error::typed("McpOAuthAuthorizationRequiredError", e.clone());
    }
    if let Some(e) = err.downcast_ref::<McpAuthRequiredError>() {
        return Error::typed("McpAuthRequiredError", e.clone());
    }
    if let Some(e) = err.downcast_ref::<McpHttpError>() {
        return Error::typed("McpHttpError", e.clone());
    }
    if let Some(e) = err.downcast_ref::<McpSessionExpiredError>() {
        return Error::typed("McpSessionExpiredError", e.clone());
    }
    Error::msg(err.message())
}

impl McpServerConnection {
    pub fn new(options: McpServerConnectionOptions) -> Arc<Self> {
        let entry = options.entry;
        let oauth_url = if entry_uses_oauth(&entry) {
            match &entry.config {
                McpServerConfig::Http(h) => Some(h.url.clone()),
                McpServerConfig::Stdio(_) => None,
            }
        } else {
            None
        };

        let conn_weak: Arc<Mutex<Option<std::sync::Weak<Self>>>> = Arc::new(Mutex::new(None));
        let auth_provider: Option<Arc<dyn McpAuthProvider>> = if let Some(url) = oauth_url {
            match options.credentials.for_server(&entry.name, &url) {
                Ok(store) => {
                    let entry_for_settings = entry.clone();
                    let weak_for_challenge = Arc::clone(&conn_weak);
                    Some(create_mcp_auth_provider(
                        url,
                        store,
                        Arc::new(move || Self::oauth_settings_for_entry(&entry_for_settings)),
                        Arc::new(move |challenge| {
                            if let Some(weak) = weak_for_challenge.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                                && let Some(conn) = weak.upgrade()
                            {
                                conn.inner.lock().unwrap_or_else(|e| e.into_inner()).challenge = Some(challenge);
                            }
                        }),
                    ))
                }
                Err(_) => None,
            }
        } else if let Some(ref provider_id) = entry.auth_provider {
            Some(Arc::new(ProviderTokenAuthAdapter {
                provider: provider_id.clone(),
                lookup: options.provider_token,
            }))
        } else {
            None
        };

        let conn = Arc::new(Self {
            entry,
            cwd: options.cwd,
            create_transport: options.create_transport,
            auth_provider,
            on_tools: options.on_tools,
            on_change: options.on_change,
            log: options.log,
            inner: Mutex::new(ConnectionMutableState {
                state: ServerState::Connecting,
                error: None,
                tools: Vec::new(),
                has_resources: false,
                resources: Vec::new(),
                resource_templates: Vec::new(),
                instructions: None,
                challenge: None,
                client: None,
                opening: None,
                closed: false,
                stderr_tail: None,
            }),
        });
        *conn_weak.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&conn));
        conn
    }

    pub fn name(&self) -> &str {
        &self.entry.name
    }

    pub fn timeout_ms(&self) -> i64 {
        let secs = match &self.entry.config {
            McpServerConfig::Stdio(s) => s.timeout,
            McpServerConfig::Http(h) => h.timeout,
        }
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        secs * 1000
    }

    pub fn oauth_url(&self) -> Option<&str> {
        if entry_uses_oauth(&self.entry)
            && let McpServerConfig::Http(ref h) = self.entry.config
        {
            return Some(&h.url);
        }
        None
    }

    pub fn state(&self) -> ServerState {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).state
    }

    pub fn error(&self) -> Option<String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).error.clone()
    }

    pub fn tools(&self) -> Vec<McpTool> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).tools.clone()
    }

    pub fn has_resources(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).has_resources
    }

    pub fn resources(&self) -> Vec<Resource> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).resources.clone()
    }

    pub fn resource_templates(&self) -> Vec<ResourceTemplate> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resource_templates
            .clone()
    }

    pub fn instructions(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .instructions
            .clone()
    }

    pub fn challenge(&self) -> Option<OAuthChallenge> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).challenge.clone()
    }

    pub fn clear_challenge(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).challenge = None;
    }

    fn oauth_settings_for_entry(entry: &McpServerEntry) -> Result<McpOAuthSettings> {
        let McpServerConfig::Http(ref http) = entry.config else {
            return Ok(McpOAuthSettings::default());
        };
        let Some(ref oauth) = http.oauth else {
            return Ok(McpOAuthSettings::default());
        };
        let client_secret = match oauth.client_secret {
            Some(ref secret) => Some(resolve_config_value_or_throw(
                secret,
                &format!("MCP server \"{}\" oauth.clientSecret", entry.name),
                None,
            )?),
            None => None,
        };
        let auth_server_metadata_url = match oauth.auth_server_metadata_url {
            Some(ref u) => Some(url::Url::parse(u).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?),
            None => None,
        };
        Ok(McpOAuthSettings {
            client_id: oauth.client_id.clone(),
            client_secret,
            callback_port: oauth.callback_port,
            callback_url: oauth.callback_url.clone(),
            scope: oauth.scope.clone(),
            client_name: oauth.client_name.clone(),
            auth_server_metadata_url,
        })
    }

    pub fn oauth_settings(&self) -> Result<McpOAuthSettings> {
        Self::oauth_settings_for_entry(&self.entry)
    }

    pub async fn get_client(self: &Arc<Self>) -> Result<Arc<McpClient>> {
        use futures::FutureExt;
        let opening_fut = {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if st.closed {
                return Err(Error::msg(format!("MCP server \"{}\" is shut down", self.entry.name)));
            }
            if let Some(ref client) = st.client
                && client.connection_state() == ClientState::Connected
            {
                return Ok(Arc::clone(client));
            }
            if let Some(ref existing) = st.opening {
                existing.clone()
            } else {
                let this = Arc::clone(self);
                let fut: BoxFuture<std::result::Result<Arc<McpClient>, Arc<Error>>> = Box::pin(async move {
                    let res = this.open().await;
                    this.inner.lock().unwrap_or_else(|e| e.into_inner()).opening = None;
                    res.map_err(Arc::new)
                });
                let shared = fut.shared();
                st.opening = Some(shared.clone());
                shared
            }
        };
        opening_fut.await.map_err(|e| clone_conn_err(&e))
    }

    async fn with_client<T, F, Fut>(self: &Arc<Self>, read_only: bool, mut run: F) -> Result<T>
    where
        F: FnMut(Arc<McpClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut attempt = 1usize;
        loop {
            let client = self.get_client().await?;
            match run(Arc::clone(&client)).await {
                Ok(val) => return Ok(val),
                Err(err) => {
                    if read_only
                        && attempt == 1
                        && err.downcast_ref::<McpHttpError>().is_some()
                        && is_transient_error(&err)
                    {
                        pi_js::time::sleep(CONNECT_RETRY_DELAYS_MS[0], None).await?;
                        attempt += 1;
                        continue;
                    }
                    if attempt == 1 && err.downcast_ref::<McpSessionExpiredError>().is_some() {
                        let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        if st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, &client)) {
                            st.client = None;
                        }
                        attempt += 1;
                        continue;
                    }
                    if !self.needs_sign_in(&err) {
                        return Err(err);
                    }
                    self.drop_client(&client).await;
                    self.mark_needs_auth();
                    return Err(Error::msg(sign_in_required_message(&self.entry)));
                }
            }
        }
    }

    pub async fn reconnect(self: &Arc<Self>) -> Result<()> {
        let pending = self.inner.lock().unwrap_or_else(|e| e.into_inner()).opening.clone();
        if let Some(p) = pending {
            let _ = p.await;
        }
        let current = self.inner.lock().unwrap_or_else(|e| e.into_inner()).client.clone();
        if let Some(c) = current {
            self.drop_client(&c).await;
        }
        self.get_client().await?;
        Ok(())
    }

    pub async fn sign_out(self: &Arc<Self>) {
        let pending = self.inner.lock().unwrap_or_else(|e| e.into_inner()).opening.clone();
        if let Some(p) = pending {
            let _ = p.await;
        }
        let current = self.inner.lock().unwrap_or_else(|e| e.into_inner()).client.clone();
        if let Some(c) = current {
            self.drop_client(&c).await;
        }
        let is_closed = self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed;
        if !is_closed {
            self.mark_needs_auth();
        }
    }

    fn needs_sign_in(&self, error: &Error) -> bool {
        error.downcast_ref::<McpOAuthAuthorizationRequiredError>().is_some()
            || (self.auth_provider.is_some() && error.downcast_ref::<McpAuthRequiredError>().is_some())
    }

    fn mark_needs_auth(self: &Arc<Self>) {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.state = ServerState::NeedsAuth;
            st.error = None;
        }
        self.changed();
    }

    fn changed(self: &Arc<Self>) {
        if let Some(ref cb) = self.on_change {
            cb(Arc::clone(self));
        }
    }

    async fn drop_client(self: &Arc<Self>, client: &Arc<McpClient>) {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
                st.client = None;
            }
        }
        let _ = client.close().await;
    }

    async fn open(self: &Arc<Self>) -> Result<Arc<McpClient>> {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.state = ServerState::Connecting;
        }
        self.changed();
        let retries: &[u64] = match self.entry.config {
            McpServerConfig::Http(_) => &CONNECT_RETRY_DELAYS_MS,
            McpServerConfig::Stdio(_) => &[],
        };
        let mut attempt = 0usize;
        loop {
            self.inner.lock().unwrap_or_else(|e| e.into_inner()).stderr_tail = None;
            match self.connect_once().await {
                Ok(client) => return Ok(client),
                Err(err) => {
                    let is_closed = self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed;
                    let delay = retries.get(attempt).copied();
                    if is_closed || delay.is_none() || !is_transient_error(&err) {
                        return Err(self.connect_failed(err));
                    }
                    if let Some(ms) = delay {
                        pi_js::time::sleep(ms, None).await?;
                    }
                    if self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed {
                        return Err(self.connect_failed(err));
                    }
                    attempt += 1;
                }
            }
        }
    }

    async fn connect_once(self: &Arc<Self>) -> Result<Arc<McpClient>> {
        let root_uri = url::Url::from_file_path(PathBuf::from(&self.cwd))
            .map(|u| u.to_string())
            .unwrap_or_else(|()| format!("file://{}", self.cwd));
        let root_name = pi_js::path::basename(&self.cwd, None);

        let client = Arc::new(McpClient::new(McpClientOptions {
            name: "pi".to_string(),
            version: VERSION.to_string(),
            title: None,
            capabilities: None,
            protocol_version: None,
            request_timeout_ms: Some(self.timeout_ms()),
            roots: Some(McpClientRoots::List(vec![Root {
                uri: root_uri,
                name: Some(root_name),
            }])),
        }));

        if let Some(ref log) = self.log {
            let log_c = Arc::clone(log);
            let name = self.entry.name.clone();
            let _ = client.on_notification(
                "notifications/message",
                Arc::new(move |params| {
                    log_c.write(&name, params.as_ref());
                }),
            );
        }

        let auth = self
            .auth_provider
            .as_ref()
            .map(|p| Arc::clone(p) as Arc<dyn AuthProvider + Send + Sync>);
        let mut created_transport: Option<CreatedTransport> = None;

        let res = async {
            let created = (self.create_transport)(&self.entry, &self.cwd, auth)?;
            created_transport = Some(created.clone());
            client.connect(created.as_transport()).await?;

            let weak_self = Arc::downgrade(self);
            let client_ref = Arc::clone(&client);
            let _ = client.on_notification(
                "notifications/tools/list_changed",
                Arc::new(move |_| {
                    if let Some(conn) = weak_self.upgrade() {
                        let c = Arc::clone(&client_ref);
                        tokio::spawn(async move {
                            conn.refresh_tools(&c).await;
                        });
                    }
                }),
            );

            let weak_self = Arc::downgrade(self);
            let client_ref = Arc::clone(&client);
            let _ = client.on_notification(
                "notifications/resources/list_changed",
                Arc::new(move |_| {
                    if let Some(conn) = weak_self.upgrade() {
                        let c = Arc::clone(&client_ref);
                        tokio::spawn(async move {
                            conn.refresh_resources(&c).await;
                        });
                    }
                }),
            );

            let weak_self = Arc::downgrade(self);
            let client_ref = Arc::clone(&client);
            let transport_for_close = created.clone();
            let _ = client.on_close(Arc::new(move || {
                if let Some(conn) = weak_self.upgrade() {
                    conn.handle_client_close(&client_ref, transport_for_close.stderr_tail());
                }
            }));

            let caps = client.server_capabilities();
            let has_tools_cap = caps.as_ref().and_then(|c| c.tools.as_ref()).is_some();
            let has_resources = caps.as_ref().and_then(|c| c.resources.as_ref()).is_some();

            let tools = if has_tools_cap {
                client.list_tools(None).await?
            } else {
                Vec::new()
            };
            let (resources, templates) = if has_resources {
                fetch_resources(&client).await
            } else {
                (Vec::new(), Vec::new())
            };

            let is_closed = self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed;
            if is_closed {
                return Err(Error::msg("shut down while connecting"));
            }
            if client.connection_state() != ClientState::Connected {
                return Err(Error::msg("connection closed during setup"));
            }

            {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                st.client = Some(Arc::clone(&client));
                st.tools = tools;
                st.has_resources = has_resources;
                st.resources = resources;
                st.resource_templates = templates;
                st.instructions = client
                    .instructions()
                    .map(|s| pi_js::str16::trim(&s).to_string())
                    .filter(|s| !s.is_empty());
                st.state = ServerState::Connected;
                st.error = None;
            }
            (self.on_tools)(Arc::clone(self));
            self.changed();
            Ok(Arc::clone(&client))
        }
        .await;

        if res.is_err() {
            let _ = client.close().await;
            if let Some(tail) = created_transport.as_ref().and_then(CreatedTransport::stderr_tail) {
                self.inner.lock().unwrap_or_else(|e| e.into_inner()).stderr_tail = Some(tail);
            }
        }
        res
    }

    fn connect_failed(self: &Arc<Self>, error: Error) -> Error {
        let is_closed = self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed;
        if self.needs_sign_in(&error) && !is_closed {
            self.mark_needs_auth();
            return Error::msg(sign_in_required_message(&self.entry));
        }
        let formatted_err = {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.state = if st.closed {
                ServerState::Closed
            } else {
                ServerState::Failed
            };
            let msg = match st.stderr_tail {
                Some(ref tail) => format!("{}\n{tail}", error.message()),
                None => error.message().to_string(),
            };
            st.error = Some(msg.clone());
            msg
        };
        self.changed();
        Error::msg(format!(
            "MCP server \"{}\" failed to connect: {formatted_err}",
            self.entry.name
        ))
    }

    fn handle_client_close(self: &Arc<Self>, client: &Arc<McpClient>, stderr_tail: Option<String>) {
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if st.closed || !st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
                return;
            }
            st.client = None;
            st.state = ServerState::Disconnected;
            st.error = Some(match stderr_tail {
                Some(tail) => format!("Connection closed\n{tail}"),
                None => "Connection closed".to_string(),
            });
        }
        self.changed();
    }

    async fn refresh_tools(self: &Arc<Self>, client: &Arc<McpClient>) {
        match client.list_tools(None).await {
            Ok(tools) => {
                let mut invoke_tools = false;
                {
                    let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if !st.closed && st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
                        st.tools = tools;
                        invoke_tools = true;
                    }
                }
                if invoke_tools {
                    (self.on_tools)(Arc::clone(self));
                }
            }
            Err(err) => {
                let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                st.error = Some(format!("Failed to refresh tools: {}", err.message()));
            }
        }
        self.changed();
    }

    async fn refresh_resources(self: &Arc<Self>, client: &Arc<McpClient>) {
        let (resources, templates) = fetch_resources(client).await;
        let mut invoke_tools = false;
        {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !st.closed && st.client.as_ref().is_some_and(|c| Arc::ptr_eq(c, client)) {
                st.resources = resources;
                st.resource_templates = templates;
                invoke_tools = true;
            }
        }
        if invoke_tools {
            (self.on_tools)(Arc::clone(self));
        }
        self.changed();
    }

    pub async fn close(self: &Arc<Self>) {
        let client_opt = {
            let mut st = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            st.closed = true;
            st.state = ServerState::Closed;
            st.client.take()
        };
        self.changed();
        if let Some(client) = client_opt {
            let _ = client.close().await;
        }
        if let Some(ref provider) = self.auth_provider {
            provider.settled().await;
        }
    }
}

#[async_trait]
impl McpToolCaller for Arc<McpServerConnection> {
    async fn call_tool(
        &self,
        name: &str,
        args: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<CallToolResult> {
        let name = name.to_string();
        self.with_client(false, move |client| {
            let n = name.clone();
            let a = args.clone();
            let o = options.clone();
            async move { client.call_tool(&n, a, o).await }
        })
        .await
    }
}

#[async_trait]
impl McpResourceServer for Arc<McpServerConnection> {
    fn name(&self) -> &str {
        McpServerConnection::name(self)
    }

    fn timeout_ms(&self) -> i64 {
        McpServerConnection::timeout_ms(self)
    }

    async fn resources_page(&self, cursor: Option<&str>, options: McpRequestOptions) -> Result<ListResourcesResult> {
        let cursor = cursor.map(String::from);
        self.with_client(true, move |client| {
            let c = cursor.clone();
            let o = options.clone();
            async move { client.list_resources_page(c.as_deref(), Some(o)).await }
        })
        .await
    }

    async fn resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult> {
        let cursor = cursor.map(String::from);
        self.with_client(true, move |client| {
            let c = cursor.clone();
            let o = options.clone();
            async move {
                without_templates(
                    || client.list_resource_templates_page(c.as_deref(), Some(o)),
                    ListResourceTemplatesResult {
                        resource_templates: Vec::new(),
                        next_cursor: None,
                        meta: None,
                    },
                )
                .await
            }
        })
        .await
    }

    async fn all_resources(&self, options: McpRequestOptions) -> Result<Vec<Resource>> {
        self.with_client(true, move |client| {
            let o = options.clone();
            async move { client.list_resources(Some(o)).await }
        })
        .await
    }

    async fn all_resource_templates(&self, options: McpRequestOptions) -> Result<Vec<ResourceTemplate>> {
        self.with_client(true, move |client| {
            let o = options.clone();
            async move { list_templates(&client, Some(o)).await }
        })
        .await
    }

    async fn read_resource(&self, uri: &str, options: McpRequestOptions) -> Result<ReadResourceResult> {
        let uri = uri.to_string();
        self.with_client(true, move |client| {
            let u = uri.clone();
            let o = options.clone();
            async move { client.read_resource(&u, Some(o)).await }
        })
        .await
    }
}

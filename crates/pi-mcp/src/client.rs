//! Port of packages/mcp/src/client.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Map, Value};

use pi_js::abort::{AbortController, AbortReason, AbortSignal, ListenerGuard};
use pi_js::time::Timeout;
use pi_js::{BoxFuture, Error, Result, Unsubscribe};

use crate::protocol::content::CallToolResult;
use crate::protocol::jsonrpc::{
    JSON_RPC_ERROR_CODES, JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, JsonRpcSuccessResponse, JsonRpcVersion, McpAbortError, McpConnectionClosedError,
    McpError, McpTimeoutError, is_json_rpc_id, to_error,
};
use crate::protocol::types::{
    ClientCapabilities, Implementation, InitializeResult, LATEST_PROTOCOL_VERSION, ListChangedCapability,
    ListResourceTemplatesResult, ListResourcesResult, ProgressNotification, ReadResourceResult, Resource,
    ResourceTemplate, Root, SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities, SupportedProtocolVersion, Tool,
};
use crate::transports::transport::McpTransport;

const DEFAULT_REQUEST_TIMEOUT_MS: i64 = 30_000;
const MAX_LIST_PAGES: i64 = 1_000;

// PORT: not exported from the TS module; public here because `McpClient::connection_state` returns it.
/// `"idle" | "connecting" | "connected" | "closed"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    Idle,
    Connecting,
    Connected,
    Closed,
}

// PORT: the sync provider is also a `BoxFuture` (`async { roots }`).
/// `readonly Root[] | (() => readonly Root[] | Promise<readonly Root[]>)`.
#[derive(Clone)]
pub enum McpClientRoots {
    List(Vec<Root>),
    Provider(Arc<dyn Fn() -> BoxFuture<Result<Vec<Root>>> + Send + Sync>),
}

#[derive(Clone)]
pub struct McpClientOptions {
    pub name: String,
    pub version: String,
    pub title: Option<String>,
    pub capabilities: Option<ClientCapabilities>,
    pub protocol_version: Option<SupportedProtocolVersion>,
    pub request_timeout_ms: Option<i64>,
    pub roots: Option<McpClientRoots>,
}

#[derive(Clone, Default)]
pub struct McpRequestOptions {
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<i64>,
    pub on_progress: Option<Arc<dyn Fn(ProgressNotification) + Send + Sync>>,
}

/// `{ signal: AbortSignal }` passed to a server-request handler.
#[derive(Clone)]
pub struct RequestHandlerContext {
    pub signal: AbortSignal,
}

/// `(params, context) => unknown | Promise<unknown>`. `None` params is JSON-RPC `undefined`.
pub type RequestHandler =
    Arc<dyn Fn(Option<Value>, RequestHandlerContext) -> BoxFuture<Result<Option<Value>>> + Send + Sync>;

pub type NotificationListener = Arc<dyn Fn(Option<Value>) + Send + Sync>;
pub type ErrorListener = Arc<dyn Fn(&Error) + Send + Sync>;
pub type CloseListener = Arc<dyn Fn() + Send + Sync>;

struct ListPage {
    items: Vec<Map<String, Value>>,
    next_cursor: Option<String>,
}

// PORT: `resolve` / `reject` are one oneshot; `onAbort` is the `ListenerGuard` that removes it.
struct PendingRequest {
    outcome: Option<tokio::sync::oneshot::Sender<Result<Value>>>,
    timeout_ms: i64,
    timer: Option<Timeout>,
    signal: Option<AbortSignal>,
    on_abort: Option<ListenerGuard>,
    cancellable: bool,
    on_progress: Option<Arc<dyn Fn(ProgressNotification) + Send + Sync>>,
    progress_token: Option<JsonRpcId>,
}

struct McpClientState {
    connection_state: ClientState,
    transport: Option<Arc<dyn McpTransport>>,
    next_request_id: i64,
    server_info: Option<Implementation>,
    server_capabilities: Option<ServerCapabilities>,
    instructions: Option<String>,
    protocol_version: Option<String>,
    pending: IndexMap<JsonRpcId, PendingRequest>,
    progress_requests: IndexMap<JsonRpcId, JsonRpcId>,
    incoming: IndexMap<JsonRpcId, AbortController>,
    request_handlers: IndexMap<String, RequestHandler>,
    notification_listeners: IndexMap<String, Vec<NotificationListener>>,
    error_listeners: Vec<ErrorListener>,
    close_listeners: Vec<CloseListener>,
    disposers: Vec<Unsubscribe>,
}

impl Default for McpClientState {
    fn default() -> Self {
        Self {
            connection_state: ClientState::Idle,
            transport: None,
            next_request_id: 1,
            server_info: None,
            server_capabilities: None,
            instructions: None,
            protocol_version: None,
            pending: IndexMap::new(),
            progress_requests: IndexMap::new(),
            incoming: IndexMap::new(),
            request_handlers: IndexMap::new(),
            notification_listeners: IndexMap::new(),
            error_listeners: Vec::new(),
            close_listeners: Vec::new(),
            disposers: Vec::new(),
        }
    }
}

struct McpClientInner {
    options: McpClientOptions,
    state: Mutex<McpClientState>,
}

#[derive(Clone)]
pub struct McpClient {
    inner: Arc<McpClientInner>,
}

fn invalid(message: &str) -> McpError {
    McpError::new(JSON_RPC_ERROR_CODES.invalid_request, message, None)
}

fn validate_initialize_result(value: &Value) -> Result<InitializeResult> {
    let Some(obj) = value.as_object() else {
        return Err(Error::typed("McpError", invalid("Invalid MCP initialize result")));
    };
    let valid_shape = obj.get("protocolVersion").is_some_and(Value::is_string)
        && obj.get("capabilities").is_some_and(Value::is_object)
        && obj.get("serverInfo").and_then(Value::as_object).is_some_and(|si| {
            si.get("name").is_some_and(Value::is_string) && si.get("version").is_some_and(Value::is_string)
        })
        && obj.get("instructions").is_none_or(Value::is_string);
    if !valid_shape {
        return Err(Error::typed("McpError", invalid("Invalid MCP initialize result")));
    }
    serde_json::from_value::<InitializeResult>(value.clone())
        .map_err(|_| Error::typed("McpError", invalid("Invalid MCP initialize result")))
}

/// One page of a paginated list: the items under `key`, each checked by `isItem`.
fn validate_list_page(
    method: &str,
    key: &str,
    value: &Value,
    is_item: &dyn Fn(&Map<String, Value>) -> bool,
) -> Result<ListPage> {
    let Some(obj) = value.as_object() else {
        return Err(Error::typed(
            "McpError",
            invalid(&format!("Invalid MCP {method} result")),
        ));
    };
    let Some(raw_items) = obj.get(key).and_then(Value::as_array) else {
        return Err(Error::typed(
            "McpError",
            invalid(&format!("Invalid MCP {method} result")),
        ));
    };
    let mut items = Vec::with_capacity(raw_items.len());
    for raw in raw_items {
        let Some(item_obj) = raw.as_object() else {
            return Err(Error::typed(
                "McpError",
                invalid(&format!("Invalid entry in MCP {method} result")),
            ));
        };
        if !is_item(item_obj) {
            return Err(Error::typed(
                "McpError",
                invalid(&format!("Invalid entry in MCP {method} result")),
            ));
        }
        items.push(item_obj.clone());
    }
    // Some servers end pagination with `null` or `""` instead of omitting the cursor.
    let next_cursor = match obj.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(Error::typed(
                "McpError",
                invalid(&format!("Invalid MCP {method} cursor")),
            ));
        }
    };
    Ok(ListPage { items, next_cursor })
}

fn is_tool(tool: &Map<String, Value>) -> bool {
    tool.get("name").is_some_and(Value::is_string) && tool.get("inputSchema").is_some_and(Value::is_object)
}

// `name` is required by the spec, but some servers omit it; the URI stands in.
fn is_resource(resource: &Map<String, Value>) -> bool {
    resource.get("uri").is_some_and(Value::is_string) && resource.get("name").is_none_or(Value::is_string)
}

fn is_resource_template(template: &Map<String, Value>) -> bool {
    template.get("uriTemplate").is_some_and(Value::is_string) && template.get("name").is_none_or(Value::is_string)
}

fn to_resource(item: &Map<String, Value>) -> Result<Resource> {
    let mut map = item.clone();
    if matches!(map.get("name"), None | Some(Value::Null))
        && let Some(uri) = map.get("uri").cloned()
    {
        map.insert("name".to_string(), uri);
    }
    serde_json::from_value(Value::Object(map))
        .map_err(|_| Error::typed("McpError", invalid("Invalid entry in MCP resources/list result")))
}

fn to_resource_template(item: &Map<String, Value>) -> Result<ResourceTemplate> {
    let mut map = item.clone();
    if matches!(map.get("name"), None | Some(Value::Null))
        && let Some(uri_template) = map.get("uriTemplate").cloned()
    {
        map.insert("name".to_string(), uri_template);
    }
    serde_json::from_value(Value::Object(map)).map_err(|_| {
        Error::typed(
            "McpError",
            invalid("Invalid entry in MCP resources/templates/list result"),
        )
    })
}

fn page_cursor(page: &ListPage) -> Option<String> {
    page.next_cursor.clone()
}

fn validate_read_resource_result(value: &Value) -> Result<ReadResourceResult> {
    let Some(obj) = value.as_object() else {
        return Err(Error::typed("McpError", invalid("Invalid MCP resources/read result")));
    };
    let Some(contents) = obj.get("contents").and_then(Value::as_array) else {
        return Err(Error::typed("McpError", invalid("Invalid MCP resources/read result")));
    };
    for item in contents {
        let Some(c) = item.as_object() else {
            return Err(Error::typed(
                "McpError",
                invalid("Invalid contents in MCP resources/read result"),
            ));
        };
        if !c.get("uri").is_some_and(Value::is_string)
            || (!c.get("text").is_some_and(Value::is_string) && !c.get("blob").is_some_and(Value::is_string))
        {
            return Err(Error::typed(
                "McpError",
                invalid("Invalid contents in MCP resources/read result"),
            ));
        }
    }
    serde_json::from_value::<ReadResourceResult>(value.clone())
        .map_err(|_| Error::typed("McpError", invalid("Invalid contents in MCP resources/read result")))
}

/// `content` is required by the spec, but servers that only return `structuredContent` omit it (the SDK defaults it too).
fn validate_call_tool_result(value: &Value) -> Result<CallToolResult> {
    let Some(obj) = value.as_object() else {
        return Err(Error::typed("McpError", invalid("Invalid MCP tools/call result")));
    };
    if obj.get("content").is_some_and(|v| !v.is_array()) {
        return Err(Error::typed("McpError", invalid("Invalid MCP tools/call result")));
    }
    if obj.get("structuredContent").is_some_and(|v| !v.is_object()) {
        return Err(Error::typed(
            "McpError",
            invalid("Invalid MCP tools/call structured content"),
        ));
    }
    serde_json::from_value::<CallToolResult>(value.clone())
        .map_err(|_| Error::typed("McpError", invalid("Invalid MCP tools/call result")))
}

fn format_json_rpc_id(id: &JsonRpcId) -> String {
    match id {
        JsonRpcId::String(s) => s.clone(),
        JsonRpcId::Number(n) => pi_js::json::number_to_string(*n),
    }
}

fn state_label(state: ClientState) -> &'static str {
    match state {
        ClientState::Idle => "idle",
        ClientState::Connecting => "connecting",
        ClientState::Connected => "connected",
        ClientState::Closed => "closed",
    }
}

fn format_abort_reason(reason: Option<&AbortReason>) -> String {
    match reason {
        None => "Aborted".to_string(),
        Some(r) if r.name.is_empty() => r.message.clone(),
        Some(r) => format!("{}: {}", r.name, r.message),
    }
}

impl McpClient {
    pub fn new(options: McpClientOptions) -> Self {
        let mut request_handlers: IndexMap<String, RequestHandler> = IndexMap::new();
        request_handlers.insert(
            "ping".to_string(),
            Arc::new(|_params, _ctx| Box::pin(async { Ok(Some(Value::Object(Map::new()))) })),
        );
        if let Some(roots) = options.roots.clone() {
            request_handlers.insert(
                "roots/list".to_string(),
                Arc::new(move |_params, _ctx| {
                    let roots = roots.clone();
                    Box::pin(async move {
                        let list = match roots {
                            McpClientRoots::List(items) => items,
                            McpClientRoots::Provider(provider) => provider().await?,
                        };
                        let mut map = Map::new();
                        map.insert(
                            "roots".to_string(),
                            serde_json::to_value(list).unwrap_or(Value::Array(Vec::new())),
                        );
                        Ok(Some(Value::Object(map)))
                    })
                }),
            );
        }

        let state = McpClientState {
            request_handlers,
            ..McpClientState::default()
        };
        Self {
            inner: Arc::new(McpClientInner {
                options,
                state: Mutex::new(state),
            }),
        }
    }

    pub fn options(&self) -> &McpClientOptions {
        &self.inner.options
    }

    pub fn connection_state(&self) -> ClientState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .connection_state
    }

    pub fn server_info(&self) -> Option<Implementation> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .server_info
            .clone()
    }

    pub fn server_capabilities(&self) -> Option<ServerCapabilities> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .server_capabilities
            .clone()
    }

    pub fn instructions(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .instructions
            .clone()
    }

    pub fn protocol_version(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .protocol_version
            .clone()
    }

    // Transport errors are reported only. Pending requests fail when the transport closes.
    pub async fn connect(&self, transport: Arc<dyn McpTransport>) -> Result<InitializeResult> {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.connection_state != ClientState::Idle {
                return Err(Error::msg(format!(
                    "Cannot connect MCP client in {} state",
                    state_label(state.connection_state)
                )));
            }
            state.connection_state = ClientState::Connecting;
            state.transport = Some(Arc::clone(&transport));

            let on_msg = transport.on_message(Arc::new({
                let this = self.clone();
                move |msg| this.handle_message(msg)
            }));
            let on_err = transport.on_error(Arc::new({
                let this = self.clone();
                move |err| this.emit_error(err)
            }));
            let on_close = transport.on_close(Arc::new({
                let this = self.clone();
                move || this.handle_transport_close()
            }));
            state.disposers = vec![on_msg, on_err, on_close];
        }

        let result: Result<InitializeResult> = async {
            transport.start().await?;
            let mut capabilities = self.inner.options.capabilities.clone().unwrap_or_default();
            if self.inner.options.roots.is_some() && capabilities.roots.is_none() {
                capabilities.roots = Some(ListChangedCapability::default());
            }
            let protocol_version = self
                .inner
                .options
                .protocol_version
                .map_or(LATEST_PROTOCOL_VERSION, SupportedProtocolVersion::as_str);

            let mut client_info = Map::new();
            client_info.insert("name".to_string(), Value::String(self.inner.options.name.clone()));
            client_info.insert("version".to_string(), Value::String(self.inner.options.version.clone()));
            if let Some(title) = &self.inner.options.title {
                client_info.insert("title".to_string(), Value::String(title.clone()));
            }

            let mut params = IndexMap::new();
            params.insert(
                "protocolVersion".to_string(),
                Value::String(protocol_version.to_string()),
            );
            params.insert(
                "capabilities".to_string(),
                serde_json::to_value(capabilities).unwrap_or_else(|_| Value::Object(Map::new())),
            );
            params.insert("clientInfo".to_string(), Value::Object(client_info));

            let raw = self
                .request_internal("initialize", Some(params), McpRequestOptions::default(), true)
                .await?;
            let init = validate_initialize_result(&raw)?;
            if !SUPPORTED_PROTOCOL_VERSIONS.contains(&init.protocol_version.as_str()) {
                return Err(Error::msg(format!(
                    "MCP server selected unsupported protocol version {}",
                    init.protocol_version
                )));
            }
            {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.protocol_version = Some(init.protocol_version.clone());
                state.server_info = Some(init.server_info.clone());
                state.server_capabilities = Some(init.capabilities.clone());
                state.instructions = init.instructions.clone();
            }
            transport.set_protocol_version(&init.protocol_version);
            self.notify_internal("notifications/initialized", None, true).await?;
            {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.connection_state == ClientState::Connecting {
                    state.connection_state = ClientState::Connected;
                }
            }
            Ok(init)
        }
        .await;

        if let Err(err) = result {
            let _ = self.close().await;
            return Err(err);
        }
        result
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<Value> {
        self.request_internal(method, params, options.unwrap_or_default(), false)
            .await
    }

    pub async fn notify(&self, method: &str, params: Option<IndexMap<String, Value>>) -> Result<()> {
        self.notify_internal(method, params, false).await
    }

    pub fn set_request_handler(&self, method: &str, handler: RequestHandler) -> Unsubscribe {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.request_handlers.insert(method.to_string(), Arc::clone(&handler));
        let inner = Arc::clone(&self.inner);
        let method = method.to_string();
        Unsubscribe::new(move || {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .request_handlers
                .get(&method)
                .is_some_and(|existing| Arc::ptr_eq(existing, &handler))
            {
                state.request_handlers.shift_remove(&method);
            }
        })
    }

    pub fn on_notification(&self, method: &str, listener: NotificationListener) -> Unsubscribe {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let list = state.notification_listeners.entry(method.to_string()).or_default();
        if !list.iter().any(|existing| Arc::ptr_eq(existing, &listener)) {
            list.push(Arc::clone(&listener));
        }
        let inner = Arc::clone(&self.inner);
        let method = method.to_string();
        Unsubscribe::new(move || {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(list) = state.notification_listeners.get_mut(&method) {
                list.retain(|existing| !Arc::ptr_eq(existing, &listener));
                if list.is_empty() {
                    state.notification_listeners.shift_remove(&method);
                }
            }
        })
    }

    pub fn on_error(&self, listener: ErrorListener) -> Unsubscribe {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state
            .error_listeners
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &listener))
        {
            state.error_listeners.push(Arc::clone(&listener));
        }
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .error_listeners
                .retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    /// Called once when the connection closes, whether the transport dropped or `close()` was called.
    pub fn on_close(&self, listener: CloseListener) -> Unsubscribe {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state
            .close_listeners
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &listener))
        {
            state.close_listeners.push(Arc::clone(&listener));
        }
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .close_listeners
                .retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    pub async fn ping(&self, options: Option<McpRequestOptions>) -> Result<()> {
        self.request("ping", None, options).await?;
        Ok(())
    }

    pub async fn list_tools(&self, options: Option<McpRequestOptions>) -> Result<Vec<Tool>> {
        let items = self
            .list_all("tools/list", "tools", &is_tool, options.unwrap_or_default())
            .await?;
        items
            .into_iter()
            .map(|mut map| {
                // Keep only spec-typed annotation fields; non-conformant servers may emit e.g. string booleans.
                if let Some(Value::Object(annotations)) = map.get_mut("annotations") {
                    for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
                        if let Some(val) = annotations.get(hint)
                            && !val.is_boolean()
                        {
                            annotations.shift_remove(hint);
                        }
                    }
                    if let Some(title) = annotations.get("title")
                        && !title.is_string()
                    {
                        annotations.shift_remove("title");
                    }
                }
                serde_json::from_value::<Tool>(Value::Object(map)).map_err(|e| Error::msg(e.to_string()))
            })
            .collect()
    }

    /// Every resource, following `nextCursor` through all pages.
    pub async fn list_resources(&self, options: Option<McpRequestOptions>) -> Result<Vec<Resource>> {
        let items = self
            .list_all("resources/list", "resources", &is_resource, options.unwrap_or_default())
            .await?;
        items.iter().map(to_resource).collect()
    }

    /// One page of resources, starting at `cursor`.
    pub async fn list_resources_page(
        &self,
        cursor: Option<&str>,
        options: Option<McpRequestOptions>,
    ) -> Result<ListResourcesResult> {
        let page = self
            .list_page(
                "resources/list",
                "resources",
                &is_resource,
                cursor,
                options.unwrap_or_default(),
            )
            .await?;
        Ok(ListResourcesResult {
            resources: page.items.iter().map(to_resource).collect::<Result<Vec<_>>>()?,
            next_cursor: page_cursor(&page),
            meta: None,
        })
    }

    /// Every resource template, following `nextCursor` through all pages.
    pub async fn list_resource_templates(&self, options: Option<McpRequestOptions>) -> Result<Vec<ResourceTemplate>> {
        let items = self
            .list_all(
                "resources/templates/list",
                "resourceTemplates",
                &is_resource_template,
                options.unwrap_or_default(),
            )
            .await?;
        items.iter().map(to_resource_template).collect()
    }

    /// One page of resource templates, starting at `cursor`.
    pub async fn list_resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: Option<McpRequestOptions>,
    ) -> Result<ListResourceTemplatesResult> {
        let page = self
            .list_page(
                "resources/templates/list",
                "resourceTemplates",
                &is_resource_template,
                cursor,
                options.unwrap_or_default(),
            )
            .await?;
        Ok(ListResourceTemplatesResult {
            resource_templates: page
                .items
                .iter()
                .map(to_resource_template)
                .collect::<Result<Vec<_>>>()?,
            next_cursor: page_cursor(&page),
            meta: None,
        })
    }

    pub async fn read_resource(&self, uri: &str, options: Option<McpRequestOptions>) -> Result<ReadResourceResult> {
        let mut params = IndexMap::new();
        params.insert("uri".to_string(), Value::String(uri.to_string()));
        let val = self.request("resources/read", Some(params), options).await?;
        validate_read_resource_result(&val)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<CallToolResult> {
        let mut params = IndexMap::new();
        params.insert("name".to_string(), Value::String(name.to_string()));
        if let Some(a) = args {
            let mut arg_map = Map::new();
            for (k, v) in a {
                arg_map.insert(k, v);
            }
            params.insert("arguments".to_string(), Value::Object(arg_map));
        }
        let val = self.request("tools/call", Some(params), options).await?;
        validate_call_tool_result(&val)
    }

    pub async fn close(&self) -> Result<()> {
        let transport = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transport
            .take();
        self.dispose_transport_listeners();
        self.mark_closed(Error::typed(
            "McpConnectionClosedError",
            McpConnectionClosedError::new(None),
        ));
        if let Some(transport) = transport {
            transport.close().await?;
        }
        Ok(())
    }

    async fn list_page(
        &self,
        method: &str,
        key: &str,
        is_item: &(dyn Fn(&Map<String, Value>) -> bool + Sync),
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListPage> {
        let params = cursor.map(|c| {
            let mut map = IndexMap::new();
            map.insert("cursor".to_string(), Value::String(c.to_string()));
            map
        });
        let value = self.request(method, params, Some(options)).await?;
        validate_list_page(method, key, &value, is_item)
    }

    /// Every item of a paginated list method.
    async fn list_all(
        &self,
        method: &str,
        key: &str,
        is_item: &(dyn Fn(&Map<String, Value>) -> bool + Sync),
        options: McpRequestOptions,
    ) -> Result<Vec<Map<String, Value>>> {
        let mut items = Vec::new();
        let mut cursors = indexmap::IndexSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = self
                .list_page(method, key, is_item, cursor.as_deref(), options.clone())
                .await?;
            items.extend(page.items);
            let Some(next) = page.next_cursor else {
                return Ok(items);
            };
            if !cursors.insert(next.clone()) {
                return Err(Error::msg(format!("MCP {method} returned duplicate cursor: {next}")));
            }
            cursor = Some(next);
        }
        Err(Error::msg(format!("MCP {method} exceeded {MAX_LIST_PAGES} pages")))
    }

    // The spec forbids cancelling `initialize`.
    async fn request_internal(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        options: McpRequestOptions,
        allow_connecting: bool,
    ) -> Result<Value> {
        let transport = self.require_transport(allow_connecting)?;
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(Error::typed(McpAbortError::NAME, McpAbortError::new(None)));
        }

        let (id, timeout_ms, progress_token, rx) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let raw_id = state.next_request_id;
            state.next_request_id += 1;
            let id = JsonRpcId::Number(raw_id as f64);
            let progress_token = options.on_progress.as_ref().map(|_| id.clone());
            let timeout_ms = options
                .timeout_ms
                .or(self.inner.options.request_timeout_ms)
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let entry = PendingRequest {
                outcome: Some(tx),
                timeout_ms,
                timer: None,
                signal: options.signal.clone(),
                on_abort: None,
                cancellable: method != "initialize",
                on_progress: options.on_progress.clone(),
                progress_token: progress_token.clone(),
            };
            state.pending.insert(id.clone(), entry);
            if let Some(token) = &progress_token {
                state.progress_requests.insert(token.clone(), id.clone());
            }
            (id, timeout_ms, progress_token, rx)
        };

        let request_params = if let Some(token) = &progress_token {
            let mut map = Map::new();
            let mut existing_meta = Map::new();
            if let Some(p) = params {
                for (k, v) in p {
                    if k == "_meta" {
                        if let Value::Object(m) = v {
                            existing_meta = m;
                        }
                    } else {
                        map.insert(k, v);
                    }
                }
            }
            existing_meta.insert(
                "progressToken".to_string(),
                match token {
                    JsonRpcId::String(s) => Value::String(s.clone()),
                    JsonRpcId::Number(n) => pi_js::json::number_value(*n),
                },
            );
            map.insert("_meta".to_string(), Value::Object(existing_meta));
            Some(Value::Object(map))
        } else {
            params.map(|p| {
                let mut map = Map::new();
                for (k, v) in p {
                    map.insert(k, v);
                }
                Value::Object(map)
            })
        };

        let message = JsonRpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: id.clone(),
            method: method.to_string(),
            params: request_params,
        };

        if let Some(signal) = &options.signal {
            let cancellable = method != "initialize";
            let this_for_abort = self.clone();
            let id_for_abort = id.clone();
            let guard = signal.on_abort(move |reason| {
                this_for_abort.cancel_pending(
                    id_for_abort,
                    Error::typed(McpAbortError::NAME, McpAbortError::new(None)),
                    cancellable,
                    Some(&format_abort_reason(Some(reason))),
                );
            });
            let dropped_guard = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = state.pending.get_mut(&id) {
                    entry.on_abort = Some(guard);
                    None
                } else {
                    Some(guard)
                }
            };
            drop(dropped_guard);
            if signal.aborted() {
                self.cancel_pending(
                    id.clone(),
                    Error::typed(McpAbortError::NAME, McpAbortError::new(None)),
                    cancellable,
                    Some(&format_abort_reason(signal.reason().as_ref())),
                );
                return rx
                    .await
                    .unwrap_or_else(|_| Err(Error::typed(McpAbortError::NAME, McpAbortError::new(None))));
            }
        }

        if timeout_ms > 0 {
            self.arm_timeout(&id);
        }

        let this_for_send = self.clone();
        let id_for_send = id.clone();
        tokio::spawn(async move {
            if let Err(err) = transport.send(JsonRpcMessage::Request(message)).await {
                this_for_send.cancel_pending(id_for_send, err, false, None);
            }
        });

        rx.await.unwrap_or_else(|_| {
            Err(Error::typed(
                "McpConnectionClosedError",
                McpConnectionClosedError::new(None),
            ))
        })
    }

    async fn notify_internal(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        allow_connecting: bool,
    ) -> Result<()> {
        let transport = self.require_transport(allow_connecting)?;
        let params_value = params.map(|p| {
            let mut map = Map::new();
            for (k, v) in p {
                map.insert(k, v);
            }
            Value::Object(map)
        });
        transport
            .send(JsonRpcMessage::Notification(JsonRpcNotification {
                jsonrpc: JsonRpcVersion::V2,
                method: method.to_string(),
                params: params_value,
            }))
            .await
    }

    fn require_transport(&self, allow_connecting: bool) -> Result<Arc<dyn McpTransport>> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(transport) = &state.transport
            && (state.connection_state == ClientState::Connected
                || (allow_connecting && state.connection_state == ClientState::Connecting))
        {
            return Ok(Arc::clone(transport));
        }
        Err(Error::typed(
            "McpConnectionClosedError",
            McpConnectionClosedError::new(Some(&format!("MCP client is {}", state_label(state.connection_state)))),
        ))
    }

    fn handle_message(&self, message: &JsonRpcMessage) {
        match message {
            JsonRpcMessage::Response(response) => self.handle_response(response),
            JsonRpcMessage::Request(request) => {
                let this = self.clone();
                let req = request.clone();
                tokio::spawn(async move {
                    this.handle_request(&req).await;
                });
            }
            JsonRpcMessage::Notification(notification) => {
                self.handle_notification(&notification.method, notification.params.as_ref());
            }
        }
    }

    fn handle_response(&self, message: &JsonRpcResponse) {
        let id = match message {
            JsonRpcResponse::Success(s) => &s.id,
            JsonRpcResponse::Error(e) => &e.id,
        };
        let Some(mut entry) = self.remove_pending(id) else {
            self.emit_error(&Error::msg(format!(
                "Received response for unknown MCP request {}",
                format_json_rpc_id(id)
            )));
            return;
        };
        if let Some(outcome) = entry.outcome.take() {
            let res = match message {
                JsonRpcResponse::Success(s) => Ok(s.result.clone()),
                JsonRpcResponse::Error(e) => Err(Error::typed(
                    "McpError",
                    McpError::new(e.error.code, e.error.message.clone(), e.error.data.clone()),
                )),
            };
            let _ = outcome.send(res);
        }
    }

    async fn handle_request(&self, message: &JsonRpcRequest) {
        let (transport, handler) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.transport.clone(),
                state.request_handlers.get(&message.method).cloned(),
            )
        };
        let Some(transport) = transport else {
            return;
        };
        let Some(handler) = handler else {
            let resp = JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
                jsonrpc: JsonRpcVersion::V2,
                id: message.id.clone(),
                error: JsonRpcErrorObject {
                    code: JSON_RPC_ERROR_CODES.method_not_found,
                    message: format!("Method not found: {}", message.method),
                    data: None,
                },
            }));
            if let Err(err) = transport.send(resp).await {
                self.emit_error(&err);
            }
            return;
        };

        let controller = AbortController::new();
        let signal = controller.signal();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .incoming
                .insert(message.id.clone(), controller);
        }

        let handler_result = handler(message.params.clone(), RequestHandlerContext { signal }).await;
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .incoming
                .shift_remove(&message.id);
        }

        let response = match handler_result {
            Ok(result) => JsonRpcMessage::Response(JsonRpcResponse::Success(JsonRpcSuccessResponse {
                jsonrpc: JsonRpcVersion::V2,
                id: message.id.clone(),
                result: result.unwrap_or_else(|| Value::Object(Map::new())),
            })),
            Err(err) => {
                let error_obj = if let Some(mcp_err) = err.downcast_ref::<McpError>() {
                    JsonRpcErrorObject {
                        code: mcp_err.code,
                        message: mcp_err.message.clone(),
                        data: mcp_err.data.clone(),
                    }
                } else {
                    JsonRpcErrorObject {
                        code: JSON_RPC_ERROR_CODES.internal_error,
                        message: err.to_string(),
                        data: None,
                    }
                };
                JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id: message.id.clone(),
                    error: error_obj,
                }))
            }
        };
        if let Err(send_err) = transport.send(response).await {
            self.emit_error(&send_err);
        }
    }

    fn handle_notification(&self, method: &str, params: Option<&Value>) {
        if method == "notifications/progress" {
            self.handle_progress(params);
        } else if method == "notifications/cancelled" {
            self.handle_cancelled(params);
        }
        let listeners = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.notification_listeners.get(method).cloned().unwrap_or_default()
        };
        for listener in listeners {
            listener(params.cloned());
        }
    }

    fn handle_progress(&self, params: Option<&Value>) {
        let Some(obj) = params.and_then(Value::as_object) else {
            return;
        };
        let Some(token_val) = obj.get("progressToken").filter(|v| is_json_rpc_id(v)) else {
            return;
        };
        if !obj
            .get("progress")
            .is_some_and(|v| v.as_f64().is_some_and(f64::is_finite))
        {
            return;
        }
        let Ok(token) = serde_json::from_value::<JsonRpcId>(token_val.clone()) else {
            return;
        };
        let Ok(notification) = serde_json::from_value::<ProgressNotification>(Value::Object(obj.clone())) else {
            return;
        };
        let (request_id, on_progress) = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(req_id) = state.progress_requests.get(&token) else {
                return;
            };
            let Some(entry) = state.pending.get(req_id) else {
                return;
            };
            (req_id.clone(), entry.on_progress.clone())
        };
        self.arm_timeout(&request_id);
        if let Some(cb) = on_progress {
            cb(notification);
        }
    }

    fn handle_cancelled(&self, params: Option<&Value>) {
        let Some(obj) = params.and_then(Value::as_object) else {
            return;
        };
        let Some(req_val) = obj.get("requestId").filter(|v| is_json_rpc_id(v)) else {
            return;
        };
        let Ok(req_id) = serde_json::from_value::<JsonRpcId>(req_val.clone()) else {
            return;
        };
        let controller = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .incoming
            .shift_remove(&req_id);
        if let Some(controller) = controller {
            let reason = obj.get("reason").map(|v| AbortReason::value(v.clone()));
            controller.abort(reason);
        }
    }

    fn arm_timeout(&self, id: &JsonRpcId) {
        let (timeout_ms, cancellable) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(entry) = state.pending.get_mut(id) else {
                return;
            };
            if let Some(old) = entry.timer.take() {
                old.clear();
            }
            if entry.timeout_ms <= 0 {
                return;
            }
            (entry.timeout_ms, entry.cancellable)
        };
        let this = self.clone();
        let req_id = id.clone();
        let timer = pi_js::time::set_timeout(timeout_ms as u64, move || {
            this.cancel_pending(
                req_id,
                Error::typed("McpTimeoutError", McpTimeoutError::new(timeout_ms)),
                cancellable,
                Some("Request timed out"),
            );
        });
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state.pending.get_mut(id) {
            if let Some(old) = entry.timer.take() {
                old.clear();
            }
            entry.timer = Some(timer);
        } else {
            timer.clear();
        }
    }

    fn cancel_pending(&self, id: JsonRpcId, error: Error, notify_server: bool, reason: Option<&str>) {
        let Some(mut entry) = self.remove_pending(&id) else {
            return;
        };
        if let Some(outcome) = entry.outcome.take() {
            let _ = outcome.send(Err(error));
        }
        drop(entry);
        if notify_server {
            let transport = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .transport
                .clone();
            if let Some(transport) = transport {
                let mut map = Map::new();
                map.insert(
                    "requestId".to_string(),
                    match &id {
                        JsonRpcId::String(s) => Value::String(s.clone()),
                        JsonRpcId::Number(n) => pi_js::json::number_value(*n),
                    },
                );
                if let Some(r) = reason.filter(|s| !s.is_empty()) {
                    map.insert("reason".to_string(), Value::String(r.to_string()));
                }
                let msg = JsonRpcMessage::Notification(JsonRpcNotification {
                    jsonrpc: JsonRpcVersion::V2,
                    method: "notifications/cancelled".to_string(),
                    params: Some(Value::Object(map)),
                });
                let this = self.clone();
                tokio::spawn(async move {
                    if let Err(err) = transport.send(msg).await {
                        this.emit_error(&err);
                    }
                });
            }
        }
    }

    fn remove_pending(&self, id: &JsonRpcId) -> Option<PendingRequest> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut entry = state.pending.shift_remove(id)?;
        if let Some(timer) = entry.timer.take() {
            timer.clear();
        }
        if let Some(token) = &entry.progress_token {
            state.progress_requests.shift_remove(token);
        }
        Some(entry)
    }

    fn reject_pending(&self, error: Error) {
        let entries: Vec<PendingRequest> = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.progress_requests.clear();
            state.pending.drain(..).map(|(_, e)| e).collect()
        };
        for mut entry in entries {
            if let Some(timer) = entry.timer.take() {
                timer.clear();
            }
            if let Some(outcome) = entry.outcome.take() {
                let _ = outcome.send(Err(to_error(&error)));
            }
        }
    }

    fn handle_transport_close(&self) {
        self.mark_closed(Error::typed(
            "McpConnectionClosedError",
            McpConnectionClosedError::new(None),
        ));
    }

    /// Idempotent: rejects in-flight requests, aborts server requests we are serving, and flips the state.
    fn mark_closed(&self, error: Error) {
        let (was_closed, entries, controllers, close_listeners) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let was_closed = state.connection_state == ClientState::Closed;
            state.connection_state = ClientState::Closed;
            state.progress_requests.clear();
            let entries: Vec<PendingRequest> = state.pending.drain(..).map(|(_, e)| e).collect();
            let controllers: Vec<AbortController> = state.incoming.drain(..).map(|(_, c)| c).collect();
            let close_listeners = if was_closed {
                Vec::new()
            } else {
                state.close_listeners.clone()
            };
            (was_closed, entries, controllers, close_listeners)
        };
        for mut entry in entries {
            if let Some(timer) = entry.timer.take() {
                timer.clear();
            }
            if let Some(outcome) = entry.outcome.take() {
                let _ = outcome.send(Err(to_error(&error)));
            }
        }
        let reason = AbortReason::from_error(&error);
        for controller in controllers {
            controller.abort(Some(reason.clone()));
        }
        if was_closed {
            return;
        }
        for listener in close_listeners {
            listener();
        }
    }

    fn emit_error(&self, error: &Error) {
        let listeners = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.error_listeners.clone()
        };
        for listener in listeners {
            listener(error);
        }
    }

    fn dispose_transport_listeners(&self) {
        let disposers: Vec<Unsubscribe> = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut state.disposers)
        };
        drop(disposers);
    }
}

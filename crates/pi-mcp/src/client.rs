//! Port of packages/mcp/src/client.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Map, Value};

use pi_js::abort::{AbortController, AbortSignal, ListenerGuard};
use pi_js::time::Timeout;
use pi_js::{BoxFuture, Error, Result, Unsubscribe};

use crate::protocol::content::CallToolResult;
use crate::protocol::jsonrpc::{
    JSON_RPC_ERROR_CODES, JsonRpcId, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, McpError,
};
use crate::protocol::types::{
    ClientCapabilities, Implementation, InitializeResult, ListResourceTemplatesResult, ListResourcesResult,
    ProgressNotification, ReadResourceResult, Resource, ResourceTemplate, Root, ServerCapabilities,
    SupportedProtocolVersion, Tool,
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
    todo!("port: validate_initialize_result")
}

/// One page of a paginated list: the items under `key`, each checked by `isItem`.
fn validate_list_page(
    method: &str,
    key: &str,
    value: &Value,
    is_item: &dyn Fn(&Map<String, Value>) -> bool,
) -> Result<ListPage> {
    todo!("port: validate_list_page")
}

fn is_tool(tool: &Map<String, Value>) -> bool {
    todo!("port: is_tool")
}

// `name` is required by the spec, but some servers omit it; the URI stands in.
fn is_resource(resource: &Map<String, Value>) -> bool {
    todo!("port: is_resource")
}

fn is_resource_template(template: &Map<String, Value>) -> bool {
    todo!("port: is_resource_template")
}

fn to_resource(item: &Map<String, Value>) -> Resource {
    todo!("port: to_resource")
}

fn to_resource_template(item: &Map<String, Value>) -> ResourceTemplate {
    todo!("port: to_resource_template")
}

fn page_cursor(page: &ListPage) -> Option<String> {
    page.next_cursor.clone()
}

fn validate_read_resource_result(value: &Value) -> Result<ReadResourceResult> {
    todo!("port: validate_read_resource_result")
}

/// `content` is required by the spec, but servers that only return `structuredContent` omit it (the SDK defaults it too).
fn validate_call_tool_result(value: &Value) -> Result<CallToolResult> {
    todo!("port: validate_call_tool_result")
}

impl McpClient {
    pub fn new(options: McpClientOptions) -> Self {
        todo!("port: McpClient::new")
    }

    pub fn options(&self) -> &McpClientOptions {
        &self.inner.options
    }

    pub fn connection_state(&self) -> ClientState {
        self.inner.state.lock().unwrap().connection_state
    }

    pub fn server_info(&self) -> Option<Implementation> {
        self.inner.state.lock().unwrap().server_info.clone()
    }

    pub fn server_capabilities(&self) -> Option<ServerCapabilities> {
        self.inner.state.lock().unwrap().server_capabilities.clone()
    }

    pub fn instructions(&self) -> Option<String> {
        self.inner.state.lock().unwrap().instructions.clone()
    }

    pub fn protocol_version(&self) -> Option<String> {
        self.inner.state.lock().unwrap().protocol_version.clone()
    }

    // Transport errors are reported only. Pending requests fail when the transport closes.
    pub async fn connect(&self, transport: Arc<dyn McpTransport>) -> Result<InitializeResult> {
        todo!("port: McpClient::connect")
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<Value> {
        todo!("port: McpClient::request")
    }

    pub async fn notify(&self, method: &str, params: Option<IndexMap<String, Value>>) -> Result<()> {
        todo!("port: McpClient::notify")
    }

    pub fn set_request_handler(&self, method: &str, handler: RequestHandler) -> Unsubscribe {
        todo!("port: McpClient::set_request_handler")
    }

    pub fn on_notification(&self, method: &str, listener: NotificationListener) -> Unsubscribe {
        todo!("port: McpClient::on_notification")
    }

    pub fn on_error(&self, listener: ErrorListener) -> Unsubscribe {
        todo!("port: McpClient::on_error")
    }

    /// Called once when the connection closes, whether the transport dropped or `close()` was called.
    pub fn on_close(&self, listener: CloseListener) -> Unsubscribe {
        todo!("port: McpClient::on_close")
    }

    pub async fn ping(&self, options: Option<McpRequestOptions>) -> Result<()> {
        todo!("port: McpClient::ping")
    }

    pub async fn list_tools(&self, options: Option<McpRequestOptions>) -> Result<Vec<Tool>> {
        todo!("port: McpClient::list_tools")
    }

    /// Every resource, following `nextCursor` through all pages.
    pub async fn list_resources(&self, options: Option<McpRequestOptions>) -> Result<Vec<Resource>> {
        todo!("port: McpClient::list_resources")
    }

    /// One page of resources, starting at `cursor`.
    pub async fn list_resources_page(
        &self,
        cursor: Option<&str>,
        options: Option<McpRequestOptions>,
    ) -> Result<ListResourcesResult> {
        todo!("port: McpClient::list_resources_page")
    }

    /// Every resource template, following `nextCursor` through all pages.
    pub async fn list_resource_templates(&self, options: Option<McpRequestOptions>) -> Result<Vec<ResourceTemplate>> {
        todo!("port: McpClient::list_resource_templates")
    }

    /// One page of resource templates, starting at `cursor`.
    pub async fn list_resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: Option<McpRequestOptions>,
    ) -> Result<ListResourceTemplatesResult> {
        todo!("port: McpClient::list_resource_templates_page")
    }

    pub async fn read_resource(&self, uri: &str, options: Option<McpRequestOptions>) -> Result<ReadResourceResult> {
        todo!("port: McpClient::read_resource")
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: Option<IndexMap<String, Value>>,
        options: Option<McpRequestOptions>,
    ) -> Result<CallToolResult> {
        todo!("port: McpClient::call_tool")
    }

    pub async fn close(&self) -> Result<()> {
        todo!("port: McpClient::close")
    }

    async fn list_page(
        &self,
        method: &str,
        key: &str,
        is_item: &(dyn Fn(&Map<String, Value>) -> bool + Sync),
        cursor: Option<&str>,
        options: McpRequestOptions,
    ) -> Result<ListPage> {
        todo!("port: McpClient::list_page")
    }

    /// Every item of a paginated list method.
    async fn list_all(
        &self,
        method: &str,
        key: &str,
        is_item: &(dyn Fn(&Map<String, Value>) -> bool + Sync),
        options: McpRequestOptions,
    ) -> Result<Vec<Map<String, Value>>> {
        todo!("port: McpClient::list_all")
    }

    // The spec forbids cancelling `initialize`.
    async fn request_internal(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        options: McpRequestOptions,
        allow_connecting: bool,
    ) -> Result<Value> {
        todo!("port: McpClient::request_internal")
    }

    async fn notify_internal(
        &self,
        method: &str,
        params: Option<IndexMap<String, Value>>,
        allow_connecting: bool,
    ) -> Result<()> {
        todo!("port: McpClient::notify_internal")
    }

    fn require_transport(&self, allow_connecting: bool) -> Result<Arc<dyn McpTransport>> {
        todo!("port: McpClient::require_transport")
    }

    fn handle_message(&self, message: &JsonRpcMessage) {
        todo!("port: McpClient::handle_message")
    }

    fn handle_response(&self, message: &JsonRpcResponse) {
        todo!("port: McpClient::handle_response")
    }

    async fn handle_request(&self, message: &JsonRpcRequest) {
        todo!("port: McpClient::handle_request")
    }

    fn handle_notification(&self, method: &str, params: Option<&Value>) {
        todo!("port: McpClient::handle_notification")
    }

    fn handle_progress(&self, params: Option<&Value>) {
        todo!("port: McpClient::handle_progress")
    }

    fn handle_cancelled(&self, params: Option<&Value>) {
        todo!("port: McpClient::handle_cancelled")
    }

    fn arm_timeout(&self, id: &JsonRpcId) {
        todo!("port: McpClient::arm_timeout")
    }

    fn cancel_pending(&self, id: JsonRpcId, error: Error, notify_server: bool, reason: Option<&str>) {
        todo!("port: McpClient::cancel_pending")
    }

    fn remove_pending(&self, id: &JsonRpcId) {
        todo!("port: McpClient::remove_pending")
    }

    fn reject_pending(&self, error: Error) {
        todo!("port: McpClient::reject_pending")
    }

    fn handle_transport_close(&self) {
        todo!("port: McpClient::handle_transport_close")
    }

    /// Idempotent: rejects in-flight requests, aborts server requests we are serving, and flips the state.
    fn mark_closed(&self, error: Error) {
        todo!("port: McpClient::mark_closed")
    }

    fn emit_error(&self, error: &Error) {
        todo!("port: McpClient::emit_error")
    }

    fn dispose_transport_listeners(&self) {
        todo!("port: McpClient::dispose_transport_listeners")
    }
}

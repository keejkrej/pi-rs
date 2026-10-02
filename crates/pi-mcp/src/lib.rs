//! Port of packages/mcp/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod auth_provider;
pub mod client;
pub mod oauth;
pub mod protocol;
pub mod testing;
pub mod transports;
// @generated-mods end
pub use pi_js::{Error, Result};

pub use crate::auth_provider::{AuthProvider, McpFetch, UnauthorizedContext};
pub use crate::client::{McpClient, McpClientOptions, McpRequestOptions};
pub use crate::protocol::content::{
    AudioContent, BlobResourceContents, CallToolResult, ContentAnnotations, ContentBlock, EmbeddedResourceContent,
    ImageContent, LlmContent, ResourceLinkContent, TextContent, TextResourceContents, to_llm_content,
};
pub use crate::protocol::jsonrpc::{
    JSON_RPC_ERROR_CODES, JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, JsonRpcSuccessResponse, McpAbortError, McpConnectionClosedError, McpError,
    McpTimeoutError, is_json_rpc_notification, is_json_rpc_request, is_json_rpc_response, parse_json_rpc_message,
};
pub use crate::protocol::types::{
    CancelledNotification, ClientCapabilities, Implementation, InitializeParams, InitializeResult,
    LATEST_PROTOCOL_VERSION, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, ProgressNotification,
    ReadResourceResult, Resource, ResourceTemplate, Root, SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities,
    SupportedProtocolVersion, Tool, ToolAnnotations, ToolExecution,
};
pub use crate::transports::stdio::{StdioTransport, StdioTransportOptions};
pub use crate::transports::streamable_http::{
    McpAuthRequiredError, McpHttpError, McpSessionExpiredError, StreamableHttpTransport, StreamableHttpTransportOptions,
};
pub use crate::transports::transport::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener, McpTransportMessageListener,
};

#[doc(hidden)]
pub use crate::testing::{InMemoryTransport, create_in_memory_transport_pair};

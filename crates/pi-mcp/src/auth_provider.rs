//! Port of packages/mcp/src/auth-provider.ts

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Body, Headers, Response};
use pi_js::{BoxFuture, Result};

// PORT: DOM `RequestInit` is not a pi-js type. Header records and `URLSearchParams` bodies are
// converted to `Headers` and `Body` by the caller. `None` init is the omitted argument.
/// `RequestInit` fields MCP passes to [`McpFetch`].
#[derive(Clone, Debug, Default)]
pub struct McpFetchInit {
    pub method: Option<String>,
    pub headers: Option<Headers>,
    pub body: Option<Body>,
    pub signal: Option<AbortSignal>,
}

/// `(input: string | URL, init?: RequestInit) => Promise<Response>`.
pub type McpFetch = Arc<dyn Fn(&str, Option<McpFetchInit>) -> BoxFuture<Result<Response>> + Send + Sync>;

pub struct UnauthorizedContext {
    /// The 401 response, or a 403 response whose challenge reports `insufficient_scope`.
    pub response: Response,
    /// TS `serverUrl: URL`.
    pub server_url: Url,
    pub fetch: McpFetch,
    /// Access token the rejected request carried, if any. A different current token means another request already refreshed it.
    pub token: Option<String>,
}

/// Supplies bearer tokens to an MCP HTTP transport and may refresh them after a 401 response.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn token(&self) -> Result<Option<String>>;

    // PORT: a missing JS method must not be invoked (a no-op success would retry the 401).
    /// Optional `onUnauthorized`. `None` means the method is absent.
    fn on_unauthorized(&self, context: UnauthorizedContext) -> Option<BoxFuture<Result<()>>> {
        let _ = context;
        None
    }
}

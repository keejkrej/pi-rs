//! Port of packages/mcp/src/oauth/errors.ts

#![allow(unused_variables)]

/// OAuth token-endpoint error (`error`, `error_description`, `error_uri`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthError {
    pub message: String,
    pub code: String,
    pub error_uri: Option<String>,
}

impl OAuthError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, error_uri: Option<String>) -> Self {
        let code = code.into();
        let raw_msg = message.into();
        let message = if raw_msg.is_empty() { code.clone() } else { raw_msg };
        Self {
            message,
            code,
            error_uri,
        }
    }
}

/// Authorization response `iss` does not match the authorization server (RFC 9207).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthIssuerMismatchError {
    pub message: String,
    /// `None` when no authorization server metadata was discovered.
    pub expected: Option<String>,
    /// `undefined` when an authorization response lacks the `iss` parameter its server promised (RFC 9207).
    pub received: Option<String>,
}

impl OAuthIssuerMismatchError {
    pub fn new(expected: Option<String>, received: Option<String>) -> Self {
        let format = |issuer: &Option<String>| match issuer {
            None => "none".to_string(),
            Some(v) => pi_js::json::stringify(v),
        };
        let expected_json = format(&expected);
        let received_json = format(&received);
        Self {
            message: format!("OAuth issuer mismatch: expected {expected_json}, received {received_json}"),
            expected,
            received,
        }
    }
}

/// Refusing to send OAuth credentials to a non-HTTPS endpoint that is not loopback.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthInsecureEndpointError {
    pub message: String,
    pub endpoint: String,
}

impl OAuthInsecureEndpointError {
    pub fn new(endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        Self {
            message: format!("Refusing to send OAuth credentials to non-HTTPS endpoint {endpoint}"),
            endpoint,
        }
    }
}

/// Dynamic client registration failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthRegistrationError {
    pub message: String,
    pub status: i64,
    pub body: String,
}

impl OAuthRegistrationError {
    pub fn new(status: i64, body: impl Into<String>) -> Self {
        let body = body.into();
        Self {
            message: format!("OAuth dynamic client registration failed with status {status}: {body}"),
            status,
            body,
        }
    }
}

/// `authorizeMcp` needs a user to complete the authorization redirect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpOAuthAuthorizationRequiredError {
    pub message: String,
}

impl McpOAuthAuthorizationRequiredError {
    pub fn new() -> Self {
        Self {
            message: "MCP OAuth authorization requires user interaction".to_string(),
        }
    }
}

impl Default for McpOAuthAuthorizationRequiredError {
    fn default() -> Self {
        Self::new()
    }
}

/// Thrown by `McpOAuthServerStore.withRefreshLock()` when another process holds the refresh lock for the
/// whole wait. `startMcpSignIn()` then skips the refresh and goes to the browser step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpRefreshLockBusyError {
    pub message: String,
}

impl McpRefreshLockBusyError {
    pub fn new() -> Self {
        Self {
            message: "Another process is refreshing the MCP server's tokens".to_string(),
        }
    }
}

impl Default for McpRefreshLockBusyError {
    fn default() -> Self {
        Self::new()
    }
}

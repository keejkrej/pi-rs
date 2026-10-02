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
        todo!("port: OAuthError::new")
    }
}

/// Authorization response `iss` does not match the authorization server (RFC 9207).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthIssuerMismatchError {
    pub message: String,
    pub expected: String,
    /// `undefined` when an authorization response lacks the `iss` parameter its server promised (RFC 9207).
    pub received: Option<String>,
}

impl OAuthIssuerMismatchError {
    pub fn new(expected: impl Into<String>, received: Option<String>) -> Self {
        todo!("port: OAuthIssuerMismatchError::new")
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
        todo!("port: OAuthInsecureEndpointError::new")
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
        todo!("port: OAuthRegistrationError::new")
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

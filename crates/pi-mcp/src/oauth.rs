//! Port of packages/mcp/src/oauth/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod callback;
pub mod discovery;
pub mod errors;
pub mod flow;
pub mod provider;
pub mod types;
// @generated-mods end

pub use crate::oauth::callback::{OAuthCallback, OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions};
pub use crate::oauth::discovery::{
    build_authorization_server_discovery_urls, discover_authorization_server_metadata, discover_oauth_server_info,
    discover_protected_resource_metadata, parse_www_authenticate, resource_url_from_server_url, select_resource,
};
pub use crate::oauth::errors::{
    McpOAuthAuthorizationRequiredError, OAuthError, OAuthInsecureEndpointError, OAuthIssuerMismatchError,
    OAuthRegistrationError,
};
pub use crate::oauth::flow::{
    AddClientAuthentication, OAuthClientProvider, OAuthFlowOptions, OAuthFlowResult, TokenRequestOptions,
    adapt_oauth_provider, authorize_mcp, exchange_authorization_code, refresh_authorization, register_client,
    start_authorization, step_up_scope,
};
pub use crate::oauth::provider::{
    McpOAuthProvider, McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore, MemoryOAuthStateStore,
};
pub use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthClientInformation, OAuthClientInformationFull,
    OAuthClientInformationMixed, OAuthClientMetadata, OAuthDiscoveryState, OAuthProtectedResourceMetadata,
    OAuthServerInfo, OAuthTokens,
};

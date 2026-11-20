//! Port of packages/mcp/src/oauth/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod callback;
pub mod discovery;
pub mod errors;
pub mod flow;
pub mod provider;
pub mod types;
// @generated-mods end
pub mod auth_provider;
pub mod client_settings;
pub mod loopback;
pub mod sign_in;

pub use crate::oauth::auth_provider::{
    CreateMcpAuthProviderOptions, McpAuthProvider, McpSettingsFn, OnChallengeFn, RefreshStoredTokensOptions,
    create_mcp_auth_provider, refresh_stored_tokens,
};
pub use crate::oauth::callback::{OAuthCallback, OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions};
pub use crate::oauth::client_settings::{
    DEFAULT_REDIRECT_URL, LoopbackSettings, McpClientMetadataDocuments, McpClientRegistration, McpOAuthClientOptions,
    McpOAuthServerStore, McpOAuthServerStoreExt, McpOAuthSettings, OnRedirectCallback, create_oauth_client_provider,
    loopback_settings, offers_client_metadata_document, registered_redirect_urls, server_specific_path, timed_fetch,
};
pub use crate::oauth::discovery::{
    build_authorization_server_discovery_urls, discover_authorization_server_metadata, discover_oauth_server_info,
    discover_protected_resource_metadata, parse_www_authenticate, resource_url_from_server_url, select_resource,
};
pub use crate::oauth::errors::{
    McpOAuthAuthorizationRequiredError, McpRefreshLockBusyError, OAuthError, OAuthInsecureEndpointError,
    OAuthIssuerMismatchError, OAuthRegistrationError,
};
pub use crate::oauth::flow::{
    AddClientAuthentication, OAuthClientMetadataDocument, OAuthClientProvider, OAuthFlowExtraOptions, OAuthFlowOptions,
    OAuthFlowResult, TokenRequestOptions, adapt_oauth_provider, authorize_mcp, authorize_mcp_with_options,
    exchange_authorization_code, refresh_authorization, register_client, start_authorization, step_up_scope,
};
pub use crate::oauth::loopback::{
    ListenForLoopbackSignInOptions, McpLoopbackCallback, PromptForRedirectUrlFn, WaitForLoopbackSignInOptions,
    listen_for_loopback_sign_in, response_from_redirect_url, wait_for_loopback_sign_in,
};
pub use crate::oauth::provider::{
    ClientMetadataDocumentFn, McpOAuthProvider, McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore,
    MemoryOAuthStateStore,
};
pub use crate::oauth::sign_in::{
    McpAuthorizationResponse, McpSignInCancelledError, McpSignInOptions, McpSignInRedirect, McpSignInStart,
    cancellable, complete_mcp_sign_in, start_mcp_sign_in,
};
pub use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthClientInformation, OAuthClientInformationFull,
    OAuthClientInformationMixed, OAuthClientMetadata, OAuthDiscoveryState, OAuthProtectedResourceMetadata,
    OAuthServerInfo, OAuthTokens, is_http_url,
};

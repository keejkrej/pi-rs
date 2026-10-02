//! Port of packages/mcp/src/oauth/discovery.ts
//!
//! Adapted from modelcontextprotocol/typescript-sdk v1.29.0 src/client/auth.ts.
//! Copyright (c) 2024 Anthropic, PBC. Licensed under MIT; see LICENSES/.
//! Modified to remove Zod/CORS shims and enforce authorization-server issuer validation.

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

use crate::auth_provider::McpFetch;
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthProtectedResourceMetadata, OAuthServerInfo,
};
use pi_js::Result;

/// `buildAuthorizationServerDiscoveryUrls` entry. `r#type` is `"oauth"` or `"oidc"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorizationServerDiscoveryType {
    #[serde(rename = "oauth")]
    Oauth,
    #[serde(rename = "oidc")]
    Oidc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationServerDiscoveryUrl {
    pub url: url::Url,
    pub r#type: AuthorizationServerDiscoveryType,
}

#[derive(Clone, Default)]
pub struct DiscoverProtectedResourceMetadataOptions {
    pub resource_metadata_url: Option<String>,
    pub protocol_version: Option<String>,
    pub fetch: Option<McpFetch>,
}

#[derive(Clone, Default)]
pub struct DiscoverAuthorizationServerMetadataOptions {
    pub fetch: Option<McpFetch>,
    pub protocol_version: Option<String>,
    pub skip_issuer_validation: Option<bool>,
}

#[derive(Clone, Default)]
pub struct DiscoverOAuthServerInfoOptions {
    pub resource_metadata_url: Option<url::Url>,
    /// Metadata document to use instead of discovery. It is trusted as configured, so its issuer is not checked.
    pub authorization_server_metadata_url: Option<url::Url>,
    pub fetch: Option<McpFetch>,
    pub skip_issuer_validation: Option<bool>,
}

pub fn parse_www_authenticate(header: Option<&str>) -> OAuthChallenge {
    todo!("port: parse_www_authenticate")
}

pub async fn discover_protected_resource_metadata(
    server_url: &str,
    options: Option<DiscoverProtectedResourceMetadataOptions>,
) -> Result<OAuthProtectedResourceMetadata> {
    todo!("port: discover_protected_resource_metadata")
}

pub fn build_authorization_server_discovery_urls(
    authorization_server_url: &str,
) -> Vec<AuthorizationServerDiscoveryUrl> {
    todo!("port: build_authorization_server_discovery_urls")
}

pub async fn discover_authorization_server_metadata(
    authorization_server_url: &str,
    options: Option<DiscoverAuthorizationServerMetadataOptions>,
) -> Result<Option<AuthorizationServerMetadata>> {
    todo!("port: discover_authorization_server_metadata")
}

pub async fn discover_oauth_server_info(
    server_url: &str,
    options: Option<DiscoverOAuthServerInfoOptions>,
) -> Result<OAuthServerInfo> {
    todo!("port: discover_oauth_server_info")
}

pub fn resource_url_from_server_url(value: &str) -> Result<url::Url> {
    todo!("port: resource_url_from_server_url")
}

pub fn select_resource(server_url: &str, metadata: Option<&OAuthProtectedResourceMetadata>) -> Result<Option<String>> {
    todo!("port: select_resource")
}

fn discard(response: Option<&pi_js::fetch::Response>) {
    todo!("port: discard")
}

/// 4xx and 502 mean "not here", so discovery tries the next candidate URL.
fn is_discovery_miss(status: i64) -> bool {
    todo!("port: is_discovery_miss")
}

/// Path suffix for `/.well-known/<kind><path>`; empty for the root path.
fn path_suffix(pathname: &str) -> String {
    todo!("port: path_suffix")
}

fn field(header: &str, name: &str) -> Option<String> {
    todo!("port: field")
}

async fn fetch_metadata(url: &url::Url, fetch: &McpFetch, protocol_version: &str) -> Result<pi_js::fetch::Response> {
    todo!("port: fetch_metadata")
}

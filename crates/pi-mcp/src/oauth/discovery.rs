//! Port of packages/mcp/src/oauth/discovery.ts
//!
//! Adapted from modelcontextprotocol/typescript-sdk v1.29.0 src/client/auth.ts.
//! Copyright (c) 2024 Anthropic, PBC. Licensed under MIT; see LICENSES/.
//! Modified to remove Zod/CORS shims and enforce authorization-server issuer validation.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth_provider::{McpFetch, McpFetchInit};
use crate::oauth::errors::OAuthIssuerMismatchError;
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthProtectedResourceMetadata, OAuthServerInfo,
    parse_authorization_server_metadata, parse_protected_resource_metadata,
};
use crate::protocol::types::LATEST_PROTOCOL_VERSION;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::{Error, Result};

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

fn default_fetch() -> McpFetch {
    Arc::new(|url, init| {
        let url = url.to_string();
        Box::pin(async move {
            let init = init.unwrap_or_default();
            let mut req = Request::new(init.method.unwrap_or_else(|| "GET".to_string()), url);
            if let Some(headers) = init.headers {
                req = req.with_headers(headers);
            }
            if let Some(body) = init.body {
                req = req.body(body);
            }
            if let Some(signal) = init.signal {
                req = req.signal(signal);
            }
            pi_js::fetch::fetch(req).await
        })
    })
}

pub fn parse_www_authenticate(header: Option<&str>) -> OAuthChallenge {
    let Some(header) = header.filter(|h| !h.is_empty()) else {
        return OAuthChallenge {
            resource_metadata_url: None,
            scope: None,
            error: None,
            error_description: None,
        };
    };
    let scheme = pi_js::str16::trim_start(header)
        .split(|c: char| pi_js::str16::is_js_whitespace(c))
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if scheme != "bearer" && scheme != "dpop" {
        return OAuthChallenge {
            resource_metadata_url: None,
            scope: None,
            error: None,
            error_description: None,
        };
    }
    let resource_metadata_url = field(header, "resource_metadata").and_then(|u| url::Url::parse(&u).ok());
    OAuthChallenge {
        resource_metadata_url,
        scope: field(header, "scope"),
        error: field(header, "error"),
        error_description: field(header, "error_description"),
    }
}

pub async fn discover_protected_resource_metadata(
    server_url: &str,
    options: Option<DiscoverProtectedResourceMetadataOptions>,
) -> Result<OAuthProtectedResourceMetadata> {
    let options = options.unwrap_or_default();
    let server = url::Url::parse(server_url).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let fetch = options.fetch.unwrap_or_else(default_fetch);
    let version = options.protocol_version.as_deref().unwrap_or(LATEST_PROTOCOL_VERSION);

    let first_url = if let Some(ref explicit) = options.resource_metadata_url {
        url::Url::parse(explicit).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    } else {
        server
            .join(&format!(
                "/.well-known/oauth-protected-resource{}",
                path_suffix(server.path())
            ))
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?
    };

    let mut response = fetch_metadata(&first_url, &fetch, version).await?;
    if options.resource_metadata_url.is_none()
        && server.path() != "/"
        && is_discovery_miss(i64::from(response.status()))
    {
        discard(Some(&response));
        let root_url = server
            .join("/.well-known/oauth-protected-resource")
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
        response = fetch_metadata(&root_url, &fetch, version).await?;
    }
    if !response.ok() {
        let status = response.status();
        discard(Some(&response));
        return Err(Error::msg(format!(
            "HTTP {status} loading OAuth protected resource metadata"
        )));
    }
    parse_protected_resource_metadata(response.json().await?)
}

pub fn build_authorization_server_discovery_urls(
    authorization_server_url: &str,
) -> Result<Vec<AuthorizationServerDiscoveryUrl>> {
    let issuer =
        url::Url::parse(authorization_server_url).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let path = path_suffix(issuer.path());
    let mut urls = vec![
        AuthorizationServerDiscoveryUrl {
            url: issuer
                .join(&format!("/.well-known/oauth-authorization-server{path}"))
                .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?,
            r#type: AuthorizationServerDiscoveryType::Oauth,
        },
        AuthorizationServerDiscoveryUrl {
            url: issuer
                .join(&format!("/.well-known/openid-configuration{path}"))
                .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?,
            r#type: AuthorizationServerDiscoveryType::Oidc,
        },
    ];
    if !path.is_empty() {
        urls.push(AuthorizationServerDiscoveryUrl {
            url: issuer
                .join(&format!("{path}/.well-known/openid-configuration"))
                .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?,
            r#type: AuthorizationServerDiscoveryType::Oidc,
        });
    }
    Ok(urls)
}

pub async fn discover_authorization_server_metadata(
    authorization_server_url: &str,
    options: Option<DiscoverAuthorizationServerMetadataOptions>,
) -> Result<Option<AuthorizationServerMetadata>> {
    let options = options.unwrap_or_default();
    let fetch = options.fetch.unwrap_or_else(default_fetch);
    let version = options.protocol_version.as_deref().unwrap_or(LATEST_PROTOCOL_VERSION);

    for entry in build_authorization_server_discovery_urls(authorization_server_url)? {
        let response = fetch_metadata(&entry.url, &fetch, version).await?;
        if !response.ok() {
            let status = response.status();
            discard(Some(&response));
            if is_discovery_miss(i64::from(status)) {
                continue;
            }
            return Err(Error::msg(format!(
                "HTTP {status} loading authorization server metadata from {}",
                entry.url
            )));
        }
        let metadata = parse_authorization_server_metadata(response.json().await?)?;
        if !options.skip_issuer_validation.unwrap_or(false) {
            let expected = authorization_server_url;
            fn trim_slash(v: &str) -> &str {
                v.strip_suffix('/').unwrap_or(v)
            }
            if trim_slash(&metadata.issuer) != trim_slash(expected) {
                return Err(Error::typed(
                    "OAuthIssuerMismatchError",
                    OAuthIssuerMismatchError::new(expected, Some(metadata.issuer)),
                ));
            }
        }
        return Ok(Some(metadata));
    }
    Ok(None)
}

pub async fn discover_oauth_server_info(
    server_url: &str,
    options: Option<DiscoverOAuthServerInfoOptions>,
) -> Result<OAuthServerInfo> {
    let options = options.unwrap_or_default();
    let resource_metadata = match discover_protected_resource_metadata(
        server_url,
        Some(DiscoverProtectedResourceMetadataOptions {
            resource_metadata_url: options.resource_metadata_url.as_ref().map(|u| u.to_string()),
            protocol_version: None,
            fetch: options.fetch.clone(),
        }),
    )
    .await
    {
        Ok(meta) => Some(meta),
        Err(err) => {
            if err.name() == "TypeError" {
                return Err(err);
            }
            None
        }
    };

    if let Some(ref url) = options.authorization_server_metadata_url {
        let fetch = options.fetch.unwrap_or_else(default_fetch);
        let response = fetch_metadata(url, &fetch, LATEST_PROTOCOL_VERSION).await?;
        if !response.ok() {
            let status = response.status();
            discard(Some(&response));
            return Err(Error::msg(format!(
                "HTTP {status} loading authorization server metadata from {url}"
            )));
        }
        let metadata = parse_authorization_server_metadata(response.json().await?)?;
        return Ok(OAuthServerInfo {
            authorization_server_url: metadata.issuer.clone(),
            authorization_server_metadata: Some(metadata),
            resource_metadata,
        });
    }

    let authorization_server_url = match resource_metadata
        .as_ref()
        .and_then(|m| m.authorization_servers.as_ref())
        .and_then(|servers| servers.first())
        .cloned()
    {
        Some(url) => url,
        None => url::Url::parse(server_url)
            .and_then(|u| u.join("/"))
            .map(|u| u.to_string())
            .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?,
    };

    let authorization_server_metadata = discover_authorization_server_metadata(
        &authorization_server_url,
        Some(DiscoverAuthorizationServerMetadataOptions {
            fetch: options.fetch,
            protocol_version: None,
            skip_issuer_validation: options.skip_issuer_validation,
        }),
    )
    .await?;

    Ok(OAuthServerInfo {
        authorization_server_url,
        authorization_server_metadata,
        resource_metadata,
    })
}

pub fn resource_url_from_server_url(value: &str) -> Result<url::Url> {
    let mut url = url::Url::parse(value).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    url.set_fragment(None);
    Ok(url)
}

pub fn select_resource(server_url: &str, metadata: Option<&OAuthProtectedResourceMetadata>) -> Result<Option<String>> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let requested = resource_url_from_server_url(server_url)?;
    let configured =
        url::Url::parse(&metadata.resource).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    if requested.origin().ascii_serialization() != configured.origin().ascii_serialization() {
        return Err(Error::msg(format!(
            "Protected resource {} does not match MCP server {requested}",
            metadata.resource
        )));
    }
    let requested_path = if requested.path().ends_with('/') {
        requested.path().to_string()
    } else {
        format!("{}/", requested.path())
    };
    let configured_path = if configured.path().ends_with('/') {
        configured.path().to_string()
    } else {
        format!("{}/", configured.path())
    };
    if !requested_path.starts_with(&configured_path) {
        return Err(Error::msg(format!(
            "Protected resource {} does not match MCP server {requested}",
            metadata.resource
        )));
    }
    Ok(Some(metadata.resource.clone()))
}

fn discard(response: Option<&Response>) {
    if let Some(r) = response {
        r.cancel_body();
    }
}

/// 4xx and 502 mean "not here", so discovery tries the next candidate URL.
fn is_discovery_miss(status: i64) -> bool {
    (400..500).contains(&status) || status == 502
}

/// Path suffix for `/.well-known/<kind><path>`; empty for the root path.
fn path_suffix(pathname: &str) -> String {
    pathname.strip_suffix('/').unwrap_or(pathname).to_string()
}

fn field(header: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"(?i)(?:^|[,\s]){}=(?:"([^"]*)"|([^\s,]+))"#, regex::escape(name));
    let re = regex::Regex::new(&pattern).ok()?;
    let caps = re.captures(header)?;
    let val = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str()).unwrap_or("");
    if val.is_empty() { None } else { Some(val.to_string()) }
}

async fn fetch_metadata(url: &url::Url, fetch: &McpFetch, protocol_version: &str) -> Result<Response> {
    let mut headers = Headers::new();
    headers.set("Accept", "application/json");
    headers.set("MCP-Protocol-Version", protocol_version);
    fetch(
        url.as_str(),
        Some(McpFetchInit {
            method: None,
            headers: Some(headers),
            body: None,
            signal: None,
        }),
    )
    .await
}

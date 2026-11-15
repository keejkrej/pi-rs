//! Port of packages/mcp/src/oauth/types.ts
//!
//! Adapted from modelcontextprotocol/typescript-sdk v1.29.0.
//! Copyright (c) 2024 Anthropic, PBC. Licensed under MIT; see LICENSES/.
//! Modified to use dependency-free structural validation.

#![allow(dead_code, unused_variables)]

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use pi_js::Result;

/// Protected-resource metadata (RFC 9728), plus any extra keys the document carried.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthProtectedResourceMetadata {
    pub resource: String,
    #[serde(rename = "authorization_servers", default, skip_serializing_if = "Option::is_none")]
    pub authorization_servers: Option<Vec<String>>,
    #[serde(rename = "scopes_supported", default, skip_serializing_if = "Option::is_none")]
    pub scopes_supported: Option<Vec<String>>,
    // PORT: serde flatten emits extra keys after the known fields. TS `{...input, ...overrides}`
    // keeps each input key's original position.
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Authorization-server metadata, plus any extra keys the document carried.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    #[serde(rename = "authorization_endpoint")]
    pub authorization_endpoint: String,
    #[serde(rename = "token_endpoint")]
    pub token_endpoint: String,
    #[serde(rename = "registration_endpoint", default, skip_serializing_if = "Option::is_none")]
    pub registration_endpoint: Option<String>,
    #[serde(rename = "scopes_supported", default, skip_serializing_if = "Option::is_none")]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(rename = "response_types_supported")]
    pub response_types_supported: Vec<String>,
    #[serde(rename = "grant_types_supported", default, skip_serializing_if = "Option::is_none")]
    pub grant_types_supported: Option<Vec<String>>,
    #[serde(
        rename = "token_endpoint_auth_methods_supported",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(
        rename = "code_challenge_methods_supported",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(
        rename = "client_id_metadata_document_supported",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub client_id_metadata_document_supported: Option<bool>,
    /// Whether authorization responses carry an `iss` parameter (RFC 9207).
    #[serde(
        rename = "authorization_response_iss_parameter_supported",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub authorization_response_iss_parameter_supported: Option<bool>,
    /// Keys other than the fields above. See [`OAuthProtectedResourceMetadata::extra`].
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Token endpoint success body. Field order matches `parseOAuthTokens`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokens {
    #[serde(rename = "access_token")]
    pub access_token: String,
    #[serde(rename = "token_type")]
    pub token_type: String,
    #[serde(rename = "expires_in", default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(rename = "refresh_token", default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(rename = "id_token", default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientMetadata {
    #[serde(rename = "redirect_uris")]
    pub redirect_uris: Vec<String>,
    #[serde(
        rename = "token_endpoint_auth_method",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(rename = "grant_types", default, skip_serializing_if = "Option::is_none")]
    pub grant_types: Option<Vec<String>>,
    #[serde(rename = "response_types", default, skip_serializing_if = "Option::is_none")]
    pub response_types: Option<Vec<String>>,
    #[serde(rename = "client_name", default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(rename = "client_uri", default, skip_serializing_if = "Option::is_none")]
    pub client_uri: Option<String>,
    #[serde(rename = "logo_uri", default, skip_serializing_if = "Option::is_none")]
    pub logo_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts: Option<Vec<String>>,
    #[serde(rename = "tos_uri", default, skip_serializing_if = "Option::is_none")]
    pub tos_uri: Option<String>,
    #[serde(rename = "policy_uri", default, skip_serializing_if = "Option::is_none")]
    pub policy_uri: Option<String>,
    #[serde(rename = "jwks_uri", default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks: Option<Value>,
    #[serde(rename = "software_id", default, skip_serializing_if = "Option::is_none")]
    pub software_id: Option<String>,
    #[serde(rename = "software_version", default, skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    #[serde(rename = "software_statement", default, skip_serializing_if = "Option::is_none")]
    pub software_statement: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInformation {
    #[serde(rename = "client_id")]
    pub client_id: String,
    #[serde(rename = "client_secret", default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(rename = "client_id_issued_at", default, skip_serializing_if = "Option::is_none")]
    pub client_id_issued_at: Option<f64>,
    #[serde(
        rename = "client_secret_expires_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub client_secret_expires_at: Option<f64>,
}

/// `OAuthClientInformation & OAuthClientMetadata`, plus extra registration-response keys.
///
/// Field order is the client-information fields, then the client-metadata fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInformationFull {
    #[serde(rename = "client_id")]
    pub client_id: String,
    #[serde(rename = "client_secret", default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(rename = "client_id_issued_at", default, skip_serializing_if = "Option::is_none")]
    pub client_id_issued_at: Option<f64>,
    #[serde(
        rename = "client_secret_expires_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub client_secret_expires_at: Option<f64>,
    #[serde(rename = "redirect_uris")]
    pub redirect_uris: Vec<String>,
    #[serde(
        rename = "token_endpoint_auth_method",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(rename = "grant_types", default, skip_serializing_if = "Option::is_none")]
    pub grant_types: Option<Vec<String>>,
    #[serde(rename = "response_types", default, skip_serializing_if = "Option::is_none")]
    pub response_types: Option<Vec<String>>,
    #[serde(rename = "client_name", default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(rename = "client_uri", default, skip_serializing_if = "Option::is_none")]
    pub client_uri: Option<String>,
    #[serde(rename = "logo_uri", default, skip_serializing_if = "Option::is_none")]
    pub logo_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts: Option<Vec<String>>,
    #[serde(rename = "tos_uri", default, skip_serializing_if = "Option::is_none")]
    pub tos_uri: Option<String>,
    #[serde(rename = "policy_uri", default, skip_serializing_if = "Option::is_none")]
    pub policy_uri: Option<String>,
    #[serde(rename = "jwks_uri", default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks: Option<Value>,
    #[serde(rename = "software_id", default, skip_serializing_if = "Option::is_none")]
    pub software_id: Option<String>,
    #[serde(rename = "software_version", default, skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    #[serde(rename = "software_statement", default, skip_serializing_if = "Option::is_none")]
    pub software_statement: Option<String>,
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

/// `OAuthClientInformation | OAuthClientInformationFull`.
// PORT: one struct. A metadata field that is `None` is absent, which is what
// `"token_endpoint_auth_method" in information` tests after `compact` drops `undefined`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientInformationMixed {
    #[serde(rename = "client_id")]
    pub client_id: String,
    #[serde(rename = "client_secret", default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(rename = "client_id_issued_at", default, skip_serializing_if = "Option::is_none")]
    pub client_id_issued_at: Option<f64>,
    #[serde(
        rename = "client_secret_expires_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub client_secret_expires_at: Option<f64>,
    #[serde(rename = "redirect_uris", default, skip_serializing_if = "Option::is_none")]
    pub redirect_uris: Option<Vec<String>>,
    #[serde(
        rename = "token_endpoint_auth_method",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(rename = "grant_types", default, skip_serializing_if = "Option::is_none")]
    pub grant_types: Option<Vec<String>>,
    #[serde(rename = "response_types", default, skip_serializing_if = "Option::is_none")]
    pub response_types: Option<Vec<String>>,
    #[serde(rename = "client_name", default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(rename = "client_uri", default, skip_serializing_if = "Option::is_none")]
    pub client_uri: Option<String>,
    #[serde(rename = "logo_uri", default, skip_serializing_if = "Option::is_none")]
    pub logo_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts: Option<Vec<String>>,
    #[serde(rename = "tos_uri", default, skip_serializing_if = "Option::is_none")]
    pub tos_uri: Option<String>,
    #[serde(rename = "policy_uri", default, skip_serializing_if = "Option::is_none")]
    pub policy_uri: Option<String>,
    #[serde(rename = "jwks_uri", default, skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks: Option<Value>,
    #[serde(rename = "software_id", default, skip_serializing_if = "Option::is_none")]
    pub software_id: Option<String>,
    #[serde(rename = "software_version", default, skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    #[serde(rename = "software_statement", default, skip_serializing_if = "Option::is_none")]
    pub software_statement: Option<String>,
    #[serde(default, flatten)]
    pub extra: IndexMap<String, Value>,
}

impl From<OAuthClientInformation> for OAuthClientInformationMixed {
    fn from(info: OAuthClientInformation) -> Self {
        Self {
            client_id: info.client_id,
            client_secret: info.client_secret,
            client_id_issued_at: info.client_id_issued_at,
            client_secret_expires_at: info.client_secret_expires_at,
            redirect_uris: None,
            token_endpoint_auth_method: None,
            grant_types: None,
            response_types: None,
            client_name: None,
            client_uri: None,
            logo_uri: None,
            scope: None,
            contacts: None,
            tos_uri: None,
            policy_uri: None,
            jwks_uri: None,
            jwks: None,
            software_id: None,
            software_version: None,
            software_statement: None,
            extra: IndexMap::new(),
        }
    }
}

impl From<OAuthClientInformationFull> for OAuthClientInformationMixed {
    fn from(info: OAuthClientInformationFull) -> Self {
        Self {
            client_id: info.client_id,
            client_secret: info.client_secret,
            client_id_issued_at: info.client_id_issued_at,
            client_secret_expires_at: info.client_secret_expires_at,
            redirect_uris: Some(info.redirect_uris),
            token_endpoint_auth_method: info.token_endpoint_auth_method,
            grant_types: info.grant_types,
            response_types: info.response_types,
            client_name: info.client_name,
            client_uri: info.client_uri,
            logo_uri: info.logo_uri,
            scope: info.scope,
            contacts: info.contacts,
            tos_uri: info.tos_uri,
            policy_uri: info.policy_uri,
            jwks_uri: info.jwks_uri,
            jwks: info.jwks,
            software_id: info.software_id,
            software_version: info.software_version,
            software_statement: info.software_statement,
            extra: info.extra,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDiscoveryState {
    pub authorization_server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server_metadata: Option<AuthorizationServerMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<OAuthProtectedResourceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthServerInfo {
    pub authorization_server_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server_metadata: Option<AuthorizationServerMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<OAuthProtectedResourceMetadata>,
}

/// Parsed `WWW-Authenticate` challenge. Not a wire object; `resource_metadata_url` is a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthChallenge {
    pub resource_metadata_url: Option<url::Url>,
    pub scope: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

pub fn parse_protected_resource_metadata(value: Value) -> Result<OAuthProtectedResourceMetadata> {
    let input = object(&value, "OAuth protected resource metadata")?;
    let resource = match input.get("resource") {
        Some(v) => safe_url(v, "OAuth protected resource metadata resource")?,
        None => return Err(pi_js::Error::msg("Invalid OAuth protected resource metadata resource")),
    };
    let authorization_servers = match input.get("authorization_servers") {
        None => None,
        Some(v) => optional_strings(v, "authorization_servers")?
            .map(|urls| {
                urls.iter()
                    .map(|u| safe_url(&Value::String(u.clone()), "authorization server URL"))
                    .collect::<Result<Vec<String>>>()
            })
            .transpose()?,
    };
    let scopes_supported = match input.get("scopes_supported") {
        None => None,
        Some(v) => optional_strings(v, "scopes_supported")?,
    };
    let mut extra = IndexMap::new();
    for (k, v) in input {
        if !matches!(k.as_str(), "resource" | "authorization_servers" | "scopes_supported") {
            extra.insert(k.clone(), v.clone());
        }
    }
    Ok(OAuthProtectedResourceMetadata {
        resource,
        authorization_servers,
        scopes_supported,
        extra,
    })
}

pub fn parse_authorization_server_metadata(value: Value) -> Result<AuthorizationServerMetadata> {
    let input = object(&value, "authorization server metadata")?;
    let response_types = match input.get("response_types_supported") {
        Some(v) => optional_strings(v, "response_types_supported")?,
        None => None,
    }
    .ok_or_else(|| pi_js::Error::msg("Invalid response_types_supported"))?;

    let issuer = safe_url(
        input.get("issuer").unwrap_or(&Value::Null),
        "authorization server issuer",
    )?;
    let authorization_endpoint = safe_url(
        input.get("authorization_endpoint").unwrap_or(&Value::Null),
        "authorization endpoint",
    )?;
    let token_endpoint = safe_url(input.get("token_endpoint").unwrap_or(&Value::Null), "token endpoint")?;
    let registration_endpoint = optional_url(
        input.get("registration_endpoint").unwrap_or(&Value::Null),
        "registration endpoint",
    )?;
    let scopes_supported = match input.get("scopes_supported") {
        None => None,
        Some(v) => optional_strings(v, "scopes_supported")?,
    };
    let grant_types_supported = match input.get("grant_types_supported") {
        None => None,
        Some(v) => optional_strings(v, "grant_types_supported")?,
    };
    let token_endpoint_auth_methods_supported = match input.get("token_endpoint_auth_methods_supported") {
        None => None,
        Some(v) => optional_strings(v, "token_endpoint_auth_methods_supported")?,
    };
    let code_challenge_methods_supported = match input.get("code_challenge_methods_supported") {
        None => None,
        Some(v) => optional_strings(v, "code_challenge_methods_supported")?,
    };
    let client_id_metadata_document_supported = input
        .get("client_id_metadata_document_supported")
        .and_then(Value::as_bool);
    let authorization_response_iss_parameter_supported = input
        .get("authorization_response_iss_parameter_supported")
        .and_then(Value::as_bool);

    let mut extra = IndexMap::new();
    for (k, v) in input {
        if !matches!(
            k.as_str(),
            "issuer"
                | "authorization_endpoint"
                | "token_endpoint"
                | "registration_endpoint"
                | "scopes_supported"
                | "response_types_supported"
                | "grant_types_supported"
                | "token_endpoint_auth_methods_supported"
                | "code_challenge_methods_supported"
                | "client_id_metadata_document_supported"
                | "authorization_response_iss_parameter_supported"
        ) {
            extra.insert(k.clone(), v.clone());
        }
    }

    Ok(AuthorizationServerMetadata {
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        scopes_supported,
        response_types_supported: response_types,
        grant_types_supported,
        token_endpoint_auth_methods_supported,
        code_challenge_methods_supported,
        client_id_metadata_document_supported,
        authorization_response_iss_parameter_supported,
        extra,
    })
}

pub fn parse_oauth_tokens(value: Value) -> Result<OAuthTokens> {
    let input = object(&value, "OAuth token response")?;
    let expires_in = match input.get("expires_in") {
        None => None,
        Some(v) if absent(v) => None,
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .ok_or_else(|| pi_js::Error::msg("Invalid expires_in"))
            .map(Some)?,
        Some(Value::String(s)) => {
            let trimmed = pi_js::str16::trim(s);
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .ok_or_else(|| pi_js::Error::msg("Invalid expires_in"))
                .map(Some)?
        }
        Some(Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        Some(_) => return Err(pi_js::Error::msg("Invalid expires_in")),
    };
    let access_token = required_string(input.get("access_token").unwrap_or(&Value::Null), "access_token")?;
    let token_type = required_string(input.get("token_type").unwrap_or(&Value::Null), "token_type")?;
    let scope = optional_string(input.get("scope").unwrap_or(&Value::Null), "scope")?;
    let refresh_token = optional_string(input.get("refresh_token").unwrap_or(&Value::Null), "refresh_token")?;
    let id_token = optional_string(input.get("id_token").unwrap_or(&Value::Null), "id_token")?;
    Ok(OAuthTokens {
        access_token,
        token_type,
        expires_in,
        scope,
        refresh_token,
        id_token,
    })
}

pub fn parse_client_information(value: Value) -> Result<OAuthClientInformationFull> {
    let input = object(&value, "OAuth client registration response")?;
    let client_id = required_string(input.get("client_id").unwrap_or(&Value::Null), "client_id")?;
    let client_secret = optional_string(input.get("client_secret").unwrap_or(&Value::Null), "client_secret")?;
    let client_id_issued_at = input
        .get("client_id_issued_at")
        .and_then(Value::as_f64)
        .filter(|f| f.is_finite());
    let client_secret_expires_at = input
        .get("client_secret_expires_at")
        .and_then(Value::as_f64)
        .filter(|f| f.is_finite());
    let redirect_uris = match input.get("redirect_uris") {
        None => None,
        Some(v) => optional_strings(v, "redirect_uris")?,
    }
    .unwrap_or_default();

    let opt_str = |k: &str| -> Option<String> { input.get(k).and_then(Value::as_str).map(String::from) };
    let opt_strs = |k: &str| -> Option<Vec<String>> {
        input.get(k).and_then(Value::as_array).and_then(|arr| {
            arr.iter()
                .map(|item| item.as_str().map(String::from))
                .collect::<Option<Vec<String>>>()
        })
    };

    let token_endpoint_auth_method = opt_str("token_endpoint_auth_method");
    let grant_types = opt_strs("grant_types");
    let response_types = opt_strs("response_types");
    let client_name = opt_str("client_name");
    let client_uri = opt_str("client_uri");
    let logo_uri = opt_str("logo_uri");
    let scope = opt_str("scope");
    let contacts = opt_strs("contacts");
    let tos_uri = opt_str("tos_uri");
    let policy_uri = opt_str("policy_uri");
    let jwks_uri = opt_str("jwks_uri");
    let jwks = input.get("jwks").filter(|v| !v.is_null()).cloned();
    let software_id = opt_str("software_id");
    let software_version = opt_str("software_version");
    let software_statement = opt_str("software_statement");

    let mut extra = IndexMap::new();
    for (k, v) in input {
        if !matches!(
            k.as_str(),
            "client_id"
                | "client_secret"
                | "client_id_issued_at"
                | "client_secret_expires_at"
                | "redirect_uris"
                | "token_endpoint_auth_method"
                | "grant_types"
                | "response_types"
                | "client_name"
                | "client_uri"
                | "logo_uri"
                | "scope"
                | "contacts"
                | "tos_uri"
                | "policy_uri"
                | "jwks_uri"
                | "jwks"
                | "software_id"
                | "software_version"
                | "software_statement"
        ) {
            extra.insert(k.clone(), v.clone());
        }
    }

    Ok(OAuthClientInformationFull {
        client_id,
        client_secret,
        client_id_issued_at,
        client_secret_expires_at,
        redirect_uris,
        token_endpoint_auth_method,
        grant_types,
        response_types,
        client_name,
        client_uri,
        logo_uri,
        scope,
        contacts,
        tos_uri,
        policy_uri,
        jwks_uri,
        jwks,
        software_id,
        software_version,
        software_statement,
        extra,
    })
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| pi_js::Error::msg(format!("Invalid {name}")))
}

/// Treats `null` and `""` as absent: servers send them for fields they have no value for, like `scope: ""`.
fn absent(value: &Value) -> bool {
    matches!(value, Value::Null) || value.as_str().is_some_and(str::is_empty)
}

fn required_string(value: &Value, name: &str) -> Result<String> {
    match value.as_str() {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(pi_js::Error::msg(format!("Invalid {name}"))),
    }
}

fn optional_string(value: &Value, name: &str) -> Result<Option<String>> {
    if absent(value) {
        return Ok(None);
    }
    required_string(value, name).map(Some)
}

fn optional_strings(value: &Value, name: &str) -> Result<Option<Vec<String>>> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(arr) = value.as_array() else {
        return Err(pi_js::Error::msg(format!("Invalid {name}")));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(pi_js::Error::msg(format!("Invalid {name}")));
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
}

fn safe_url(value: &Value, name: &str) -> Result<String> {
    let text = required_string(value, name)?;
    let parsed = url::Url::parse(&text).map_err(|_| pi_js::Error::msg(format!("Invalid {name}")))?;
    if matches!(parsed.scheme(), "javascript" | "data" | "vbscript") {
        return Err(pi_js::Error::msg(format!("Invalid {name}")));
    }
    Ok(text)
}

fn optional_url(value: &Value, name: &str) -> Result<Option<String>> {
    if absent(value) {
        Ok(None)
    } else {
        safe_url(value, name).map(Some)
    }
}

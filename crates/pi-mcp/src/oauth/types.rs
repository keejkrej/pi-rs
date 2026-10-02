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
    todo!("port: parse_protected_resource_metadata")
}

pub fn parse_authorization_server_metadata(value: Value) -> Result<AuthorizationServerMetadata> {
    todo!("port: parse_authorization_server_metadata")
}

pub fn parse_oauth_tokens(value: Value) -> Result<OAuthTokens> {
    todo!("port: parse_oauth_tokens")
}

pub fn parse_client_information(value: Value) -> Result<OAuthClientInformationFull> {
    todo!("port: parse_client_information")
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    todo!("port: object")
}

/// Treats `null` and `""` as absent: servers send them for fields they have no value for, like `scope: ""`.
fn absent(value: &Value) -> bool {
    todo!("port: absent")
}

fn required_string(value: &Value, name: &str) -> Result<String> {
    todo!("port: required_string")
}

fn optional_string(value: &Value, name: &str) -> Result<Option<String>> {
    todo!("port: optional_string")
}

fn optional_strings(value: &Value, name: &str) -> Result<Option<Vec<String>>> {
    todo!("port: optional_strings")
}

fn safe_url(value: &Value, name: &str) -> Result<String> {
    todo!("port: safe_url")
}

fn optional_url(value: &Value, name: &str) -> Result<Option<String>> {
    todo!("port: optional_url")
}

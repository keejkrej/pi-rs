//! Port of packages/ai/src/auth/types.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};

use crate::types::{ProviderEnv, ProviderHeaders};

/// Request auth for a single model request. If a value cannot be expressed as
/// `api_key`, `headers`, or `base_url`, it is provider config, not auth.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Stored api-key credential. `env` holds provider-scoped environment/config
/// values such as Cloudflare account/gateway ids.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyCredential {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// OAuth token data returned by extension compatibility flows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: i64,
    #[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra: IndexMap<String, Value>,
}

/// Stored canonical OAuth credential.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub refresh: String,
    pub access: String,
    pub expires: i64,
    #[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra: IndexMap<String, Value>,
}

impl OAuthCredential {
    /// Construct an OAuth credential without extra provider-specific metadata.
    pub fn new(refresh: impl Into<String>, access: impl Into<String>, expires: i64) -> Self {
        Self {
            refresh: refresh.into(),
            access: access.into(),
            expires,
            extra: IndexMap::new(),
        }
    }

    /// Look up a string metadata field stored alongside the OAuth tokens.
    pub fn get_extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|v| v.as_str())
    }

    /// Insert metadata while ensuring the reserved `type` discriminant tag is never duplicated.
    pub fn set_extra(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if key != "type" {
            self.extra.insert(key, value);
        }
    }
}

impl From<OAuthCredentials> for OAuthCredential {
    fn from(mut value: OAuthCredentials) -> Self {
        value.extra.shift_remove("type");
        Self {
            refresh: value.refresh,
            access: value.access,
            expires: value.expires,
            extra: value.extra,
        }
    }
}

impl From<OAuthCredential> for OAuthCredentials {
    fn from(value: OAuthCredential) -> Self {
        Self {
            refresh: value.refresh,
            access: value.access,
            expires: value.expires,
            extra: value.extra,
        }
    }
}

/// Credential kind tag matching `"api_key" | "oauth"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
}

/// One type-tagged credential per provider — the shape of `auth.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    pub fn auth_type(&self) -> AuthType {
        match self {
            Self::ApiKey(_) => AuthType::ApiKey,
            Self::OAuth(_) => AuthType::OAuth,
        }
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self, Self::ApiKey(_))
    }

    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::OAuth(_))
    }

    pub fn as_api_key(&self) -> Option<&ApiKeyCredential> {
        match self {
            Self::ApiKey(c) => Some(c),
            Self::OAuth(_) => None,
        }
    }

    pub fn as_oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Self::OAuth(c) => Some(c),
            Self::ApiKey(_) => None,
        }
    }
}

/// Non-secret credential metadata for account/status enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub provider_id: String,
    #[serde(rename = "type")]
    pub credential_type: AuthType,
}

/// Optional cancellation for public auth and credential operations.
#[derive(Clone, Debug, Default)]
pub struct AuthOperationOptions {
    pub signal: Option<AbortSignal>,
}

/// Callback type for atomic read-modify-write mutations on [`CredentialStore`].
pub type ModifyCredentialFn = Box<dyn FnOnce(Option<Credential>) -> BoxFuture<Result<Option<Credential>>> + Send>;

/// App-owned credential storage, keyed by `Provider.id`, one credential per provider.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<Option<Credential>>;

    async fn list(&self, options: Option<AuthOperationOptions>) -> Result<Vec<CredentialInfo>>;

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyCredentialFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>>;

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> Result<()>;
}

/// Environment access for auth resolution. Injectable for tests and browsers.
#[async_trait::async_trait]
pub trait AuthContext: Send + Sync {
    async fn env(&self, name: &str) -> Option<String>;

    /// Check whether a file exists. Supports a leading `~`.
    async fn file_exists(&self, path: &str) -> bool;
}

/// Result of resolving auth for a model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResult {
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials and ambient context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI: "ANTHROPIC_API_KEY", "OAuth", "~/.aws/credentials".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "type")]
    pub auth_type: AuthType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSelectOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthPromptKind {
    Text {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Secret {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Select {
        message: String,
        options: Vec<AuthSelectOption>,
    },
    ManualCode {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
}

/// Prompt shown to the user during login.
#[derive(Clone, Debug)]
pub struct AuthPrompt {
    pub kind: AuthPromptKind,
    pub signal: Option<AbortSignal>,
}

impl AuthPrompt {
    pub fn text(message: impl Into<String>, placeholder: Option<String>) -> Self {
        Self {
            kind: AuthPromptKind::Text {
                message: message.into(),
                placeholder,
            },
            signal: None,
        }
    }

    pub fn secret(message: impl Into<String>, placeholder: Option<String>) -> Self {
        Self {
            kind: AuthPromptKind::Secret {
                message: message.into(),
                placeholder,
            },
            signal: None,
        }
    }

    pub fn select(message: impl Into<String>, options: Vec<AuthSelectOption>) -> Self {
        Self {
            kind: AuthPromptKind::Select {
                message: message.into(),
                options,
            },
            signal: None,
        }
    }

    pub fn manual_code(message: impl Into<String>, placeholder: Option<String>) -> Self {
        Self {
            kind: AuthPromptKind::ManualCode {
                message: message.into(),
                placeholder,
            },
            signal: None,
        }
    }

    pub fn with_signal(mut self, signal: AbortSignal) -> Self {
        self.signal = Some(signal);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfoLink {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEvent {
    Info {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        links: Option<Vec<AuthInfoLink>>,
    },
    AuthUrl {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    DeviceCode {
        user_code: String,
        verification_uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        open_browser_on_confirm: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        copy_code_on_confirm: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        interval_seconds: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_in_seconds: Option<f64>,
    },
    Progress {
        message: String,
    },
}

/// Login interaction callbacks serving both api-key and OAuth flows.
#[async_trait::async_trait]
pub trait AuthInteraction: Send + Sync {
    fn signal(&self) -> Option<&AbortSignal> {
        None
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String>;

    fn notify(&self, event: AuthEvent);
}

/// Normalized interaction passed to provider login implementations.
#[derive(Clone)]
pub struct ProviderAuthInteraction {
    pub inner: Arc<dyn AuthInteraction>,
    pub signal: AbortSignal,
}

impl ProviderAuthInteraction {
    pub fn new(inner: Arc<dyn AuthInteraction>, signal: AbortSignal) -> Self {
        Self { inner, signal }
    }

    pub async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        self.inner.prompt(prompt).await
    }

    pub fn notify(&self, event: AuthEvent) {
        self.inner.notify(event);
    }
}

/// Owned input passed to `ApiKeyAuth::check` and `ApiKeyAuth::resolve`.
#[derive(Clone)]
pub struct ApiKeyAuthInput {
    pub ctx: Arc<dyn AuthContext>,
    pub credential: Option<ApiKeyCredential>,
    pub signal: AbortSignal,
}

/// Api-key auth: stored key/provider env plus ambient sources (env vars, AWS
/// profiles, ADC files). Ambient-only providers return `false` from `supports_login`.
#[async_trait::async_trait]
pub trait ApiKeyAuth: Send + Sync {
    fn name(&self) -> &str;

    fn supports_login(&self) -> bool {
        false
    }

    async fn login(&self, _interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        Err(Error::msg(format!(
            "{} does not support interactive login",
            self.name()
        )))
    }

    fn supports_check(&self) -> bool {
        false
    }

    async fn check(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        Ok(None)
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>>;
}

pub type GetDeviceIdFn = Arc<dyn Fn() -> String + Send + Sync>;

/// App-supplied context for `Models::login`.
#[derive(Clone, Default)]
pub struct LoginOptions {
    pub get_device_id: Option<GetDeviceIdFn>,
    /// Name this app introduces itself with during login, e.g. OpenAI's agent name hint and
    /// Codex originator. Defaults to pi's own name.
    pub agent_name: Option<String>,
}

/// OAuth auth. The `refresh`/`to_auth` split lets `Models` own the locked
/// refresh pattern.
#[async_trait::async_trait]
pub trait OAuthAuth: Send + Sync {
    fn name(&self) -> &str;

    fn is_subscription(&self) -> bool {
        false
    }

    fn login_label(&self) -> Option<&str> {
        None
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<OAuthCredential>;

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential>;

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth>;
}

/// Provider auth. At least one of `api_key`/`oauth` must be present.
#[derive(Clone, Default)]
pub struct ProviderAuth {
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    pub oauth: Option<Arc<dyn OAuthAuth>>,
}

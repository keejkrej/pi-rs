//! Port of packages/ai/src/compat/extension-oauth-types.ts

use serde::{Deserialize, Serialize};

use pi_js::abort::AbortSignal;
use pi_js::{Error, Result};

pub use crate::auth::types::OAuthCredentials;

/// Legacy extension OAuth prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthPrompt {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty: Option<bool>,
}

/// Legacy extension OAuth authorization link.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAuthInfo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Legacy extension OAuth device-code notification.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDeviceCodeInfo {
    pub user_code: String,
    pub verification_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSelectOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSelectPrompt {
    pub message: String,
    pub options: Vec<OAuthSelectOption>,
}

/// Callback surface retained only for coding-agent extension compatibility.
#[async_trait::async_trait]
pub trait OAuthLoginCallbacks: Send + Sync {
    fn on_auth(&self, info: OAuthAuthInfo);

    fn on_device_code(&self, info: OAuthDeviceCodeInfo);

    async fn on_prompt(&self, prompt: OAuthPrompt) -> Result<String>;

    fn on_progress(&self, _message: &str) {}

    fn has_manual_code_input(&self) -> bool {
        false
    }

    async fn on_manual_code_input(&self) -> Result<String> {
        Err(Error::msg("Manual code input not supported"))
    }

    async fn on_select(&self, prompt: OAuthSelectPrompt) -> Result<Option<String>>;

    fn signal(&self) -> Option<&AbortSignal> {
        None
    }
}

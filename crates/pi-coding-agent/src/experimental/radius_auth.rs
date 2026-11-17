//! Port of packages/coding-agent/src/experimental/radius-auth.ts

use std::sync::Arc;

use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::providers::radius_config::normalize_radius_gateway_url;
use pi_js::abort::AbortSignal;
use pi_js::fs::promises::read_to_string;
use tokio::sync::OnceCell;

use crate::cli::auth_command::get_auth_credential;
use crate::cli::experimental::command_options::AuthInput;
use crate::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use crate::core::radius::get_radius_gateway_url;
use crate::utils::paths::{PathInputOptions, resolve_path};

pub use crate::core::radius::ENV_RADIUS_GATEWAY;

const RADIUS_OAUTH_MIN_VALIDITY_MS: i64 = 5 * 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusRelayAuth {
    pub gateway: String,
    pub token: String,
}

#[derive(Clone, Debug, Default)]
pub struct RadiusRelayAuthOptions {
    pub required: bool,
    pub signal: Option<AbortSignal>,
}

/// Resolve explicit or stored Radius credentials anew for every relay connection attempt.
pub struct RadiusRelayAuthResolver {
    input: Option<AuthInput>,
    gateway: String,
    model_runtime: OnceCell<Arc<ModelRuntime>>,
}

impl RadiusRelayAuthResolver {
    pub fn new(input: Option<AuthInput>, gateway: Option<&str>) -> Self {
        let raw = gateway.map(ToString::to_string).unwrap_or_else(get_radius_gateway_url);
        Self {
            input,
            gateway: normalize_radius_gateway_url(&raw),
            model_runtime: OnceCell::new(),
        }
    }

    pub fn gateway(&self) -> &str {
        &self.gateway
    }

    pub async fn resolve(&self, options: RadiusRelayAuthOptions) -> pi_js::Result<Option<RadiusRelayAuth>> {
        if let Some(ref sig) = options.signal {
            sig.throw_if_aborted()?;
        }
        if pi_js::env::var("PI_OFFLINE").is_some() {
            if options.required {
                return Err(pi_js::Error::msg(
                    "Radius relay connections are unavailable in offline mode",
                ));
            }
            return Ok(None);
        }

        if let Some(explicit) = self.explicit_token(options.signal.as_ref()).await? {
            return Ok(Some(RadiusRelayAuth {
                gateway: self.gateway.clone(),
                token: explicit,
            }));
        }

        let runtime = self
            .model_runtime
            .get_or_try_init(|| async {
                let rt = ModelRuntime::create(ModelRuntimeCreateOptions {
                    refresh_on_create: Some(false),
                    allow_model_network: Some(false),
                    ..Default::default()
                })
                .await?;
                Ok::<Arc<ModelRuntime>, pi_js::Error>(Arc::new(rt))
            })
            .await?;

        if let Some(ref sig) = options.signal {
            sig.throw_if_aborted()?;
        }

        let auth_result = runtime
            .as_models()
            .get_auth_provider_id(
                "radius",
                Some(AuthResolutionOverrides {
                    min_oauth_validity_ms: Some(RADIUS_OAUTH_MIN_VALIDITY_MS),
                    signal: options.signal,
                    ..Default::default()
                }),
            )
            .await?;

        if let Some(token) = get_auth_credential(auth_result.as_ref())
            && !token.is_empty()
        {
            return Ok(Some(RadiusRelayAuth {
                gateway: self.gateway.clone(),
                token,
            }));
        }

        if options.required {
            return Err(pi_js::Error::msg(
                "Radius authentication is required; start Pi and run /login radius, then retry",
            ));
        }

        Ok(None)
    }

    async fn explicit_token(&self, signal: Option<&AbortSignal>) -> pi_js::Result<Option<String>> {
        let Some(ref input) = self.input else {
            return Ok(None);
        };
        if let Some(sig) = signal {
            sig.throw_if_aborted()?;
        }
        let raw = match input {
            AuthInput::Token { token } => token.clone(),
            AuthInput::File { path } => {
                let resolved = resolve_path(path, None, PathInputOptions::default());
                read_with_abort(&resolved, signal).await?
            }
        };
        let token = raw.trim().to_string();
        if token.is_empty() {
            return Err(pi_js::Error::msg("Radius authentication token must not be empty"));
        }
        Ok(Some(token))
    }
}

async fn read_with_abort(path: &str, signal: Option<&AbortSignal>) -> pi_js::Result<String> {
    let Some(sig) = signal else {
        return read_to_string(path).await;
    };
    sig.throw_if_aborted()?;
    tokio::select! {
        biased;
        _ = sig.cancelled() => {
            sig.throw_if_aborted()?;
            Err(pi_js::Error::msg("The operation was aborted"))
        }
        res = read_to_string(path) => res,
    }
}

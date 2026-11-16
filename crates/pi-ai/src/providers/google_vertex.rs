//! Port of packages/ai/src/providers/google-vertex.ts

use std::sync::Arc;

use pi_js::{Error, Result};

use crate::api::google_vertex_lazy::google_vertex_api;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthEvent, AuthInfoLink, AuthPrompt, AuthResult, AuthSelectOption,
    ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::google_vertex_models::google_vertex_models;
use crate::types::{
    AnyModel, ApiStreamOptions, Model, ProviderEnv, ProviderStreams, SimpleStreamOptions, TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;

const VERTEX_ID: &str = "google-vertex";
const VERTEX_NAME: &str = "Google Vertex AI";
const VERTEX_ADC_PATH: &str = "~/.config/gcloud/application_default_credentials.json";

/// Vertex accepts an explicit API key or Application Default Credentials
/// (`gcloud auth application-default login`). ADC additionally requires
/// project and location env vars, which the implementation reads itself.
struct VertexAuth;

async fn read_auth_env(input: &ApiKeyAuthInput, name: &str) -> Result<Option<String>> {
    input.signal.throw_if_aborted()?;
    let value = input.ctx.env(name).await;
    input.signal.throw_if_aborted()?;
    Ok(value.filter(|v| !v.is_empty()))
}

#[async_trait::async_trait]
impl ApiKeyAuth for VertexAuth {
    fn name(&self) -> &str {
        "Google Cloud credentials"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        interaction.signal.throw_if_aborted()?;
        let method = interaction
            .prompt(AuthPrompt::select(
                "Select Google Vertex AI authentication method:",
                vec![
                    AuthSelectOption {
                        id: "api-key".to_owned(),
                        label: "Google Cloud API key".to_owned(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "adc".to_owned(),
                        label: "Application Default Credentials".to_owned(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "service-account".to_owned(),
                        label: "Service account credentials file".to_owned(),
                        description: None,
                    },
                ],
            ))
            .await?;
        interaction.signal.throw_if_aborted()?;

        if method == "api-key" {
            let key = interaction
                .prompt(AuthPrompt::secret("Enter Google Cloud API key", None))
                .await?;
            interaction.signal.throw_if_aborted()?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        }

        if method != "adc" && method != "service-account" {
            return Err(Error::msg(format!("Unknown Google Vertex AI auth method: {method}")));
        }

        let info_msg = if method == "adc" {
            "Run `gcloud auth application-default login`, then provide the project and location."
        } else {
            "Provide a service account credentials file, project, and location."
        };
        interaction.notify(AuthEvent::Info {
            message: info_msg.to_owned(),
            links: Some(vec![AuthInfoLink {
                label: Some("Application Default Credentials".to_owned()),
                url: "https://cloud.google.com/docs/authentication/provide-credentials-adc".to_owned(),
            }]),
        });

        let project = interaction
            .prompt(AuthPrompt::text("Enter Google Cloud project ID", None))
            .await?;
        interaction.signal.throw_if_aborted()?;
        let location = interaction
            .prompt(AuthPrompt::text("Enter Google Cloud location", None))
            .await?;
        interaction.signal.throw_if_aborted()?;
        let credentials_path = if method == "service-account" {
            let p = interaction
                .prompt(AuthPrompt::text("Enter service account credentials file path", None))
                .await?;
            interaction.signal.throw_if_aborted()?;
            Some(p)
        } else {
            None
        };

        let mut env = ProviderEnv::new();
        env.insert("GOOGLE_CLOUD_PROJECT".to_owned(), project);
        env.insert("GOOGLE_CLOUD_LOCATION".to_owned(), location);
        if let Some(path) = credentials_path {
            env.insert("GOOGLE_APPLICATION_CREDENTIALS".to_owned(), path);
        }

        Ok(ApiKeyCredential {
            key: None,
            env: Some(env),
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let had_credential = input.credential.is_some();
        let cred_key = input
            .credential
            .as_ref()
            .and_then(|c| c.key.as_ref())
            .filter(|k| !k.is_empty())
            .cloned();

        if let Some(key) = cred_key {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: Some("stored credential".to_owned()),
            }));
        }

        if let Some(env_key) = read_auth_env(&input, "GOOGLE_CLOUD_API_KEY").await? {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(env_key),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: Some("GOOGLE_CLOUD_API_KEY".to_owned()),
            }));
        }

        let cred_env = input.credential.as_ref().and_then(|c| c.env.as_ref());
        let adc_path = match cred_env
            .and_then(|e| e.get("GOOGLE_APPLICATION_CREDENTIALS"))
            .filter(|v| !v.is_empty())
            .cloned()
        {
            Some(path) => Some(path),
            None => read_auth_env(&input, "GOOGLE_APPLICATION_CREDENTIALS").await?,
        };
        input.signal.throw_if_aborted()?;
        let has_credentials = input
            .ctx
            .file_exists(adc_path.as_deref().unwrap_or(VERTEX_ADC_PATH))
            .await;
        input.signal.throw_if_aborted()?;

        let project = match cred_env
            .and_then(|e| e.get("GOOGLE_CLOUD_PROJECT"))
            .filter(|v| !v.is_empty())
            .cloned()
        {
            Some(p) => Some(p),
            None => match read_auth_env(&input, "GOOGLE_CLOUD_PROJECT").await? {
                Some(p) => Some(p),
                None => read_auth_env(&input, "GCLOUD_PROJECT").await?,
            },
        };

        let location = match cred_env
            .and_then(|e| e.get("GOOGLE_CLOUD_LOCATION"))
            .filter(|v| !v.is_empty())
            .cloned()
        {
            Some(l) => Some(l),
            None => read_auth_env(&input, "GOOGLE_CLOUD_LOCATION").await?,
        };

        if has_credentials && project.is_some() && location.is_some() {
            let source = if had_credential {
                "stored credential"
            } else {
                "gcloud application default credentials"
            };
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: input.credential.and_then(|c| c.env),
                source: Some(source.to_owned()),
            }));
        }

        Ok(None)
    }
}

struct VertexDirectFallback {
    auth: ProviderAuth,
    models: Vec<Model>,
    streams: ProviderStreams,
}

impl Provider for VertexDirectFallback {
    fn id(&self) -> &str {
        VERTEX_ID
    }

    fn name(&self) -> &str {
        VERTEX_NAME
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        Some(self.models.iter().cloned().map(AnyModel::Chat).collect())
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream_simple(model, context, options)
    }
}

pub fn google_vertex_provider() -> Arc<dyn Provider + Send + Sync> {
    let auth = ProviderAuth {
        api_key: Some(Arc::new(VertexAuth)),
        oauth: None,
    };
    let models: Vec<Model> = google_vertex_models().values().cloned().collect();
    let streams = google_vertex_api();

    match create_provider(CreateProviderOptions {
        id: VERTEX_ID.to_owned(),
        name: Some(VERTEX_NAME.to_owned()),
        base_url: None,
        headers: None,
        auth: auth.clone(),
        models: models.iter().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(streams.clone())),
        images: None,
        classifiers: None,
    }) {
        Ok(provider) => provider,
        Err(_) => Arc::new(VertexDirectFallback { auth, models, streams }),
    }
}

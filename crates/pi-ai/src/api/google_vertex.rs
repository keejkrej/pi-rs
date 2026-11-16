//! Port of packages/ai/src/api/google-vertex.ts

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use pi_js::fetch::Headers;
use pi_js::{BoxFuture, Error, Result};
use regex::Regex;
use url::Url;

use crate::api::google_shared::{
    GoogleParamsInput, GoogleStreamRequest, GoogleThinkingOptions, GoogleToolChoice, apply_on_payload,
    build_google_generate_params, build_google_headers, format_google_model_path, project_google_params_to_wire_body,
    resolve_simple_google_thinking, spawn_google_stream, to_json_value,
};
use crate::api::lazy::lazy_stream;
use crate::api::simple_options::build_base_options;
use crate::types::{
    ApiStreamOptions, JsonObject, Model, ProviderEnv, ProviderStreams, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::ProviderRetryOptions;
use crate::utils::transcript::collapse_system_messages;

const API_VERSION: &str = "v1";
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";
const VERTEX_CLOUD_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const RESOURCE_SCOPE_COLLECTION: &str = "COLLECTION";

#[derive(Clone, Default, Debug)]
pub struct GoogleVertexOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<GoogleToolChoice>,
    pub thinking: Option<GoogleThinkingOptions>,
    pub project: Option<String>,
    pub location: Option<String>,
}

impl Deref for GoogleVertexOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for GoogleVertexOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for GoogleVertexOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

impl From<GoogleVertexOptions> for ApiStreamOptions {
    fn from(options: GoogleVertexOptions) -> Self {
        let mut extra = JsonObject::new();
        if let Some(tool_choice) = options.tool_choice {
            extra.insert("toolChoice".to_owned(), to_json_value(&tool_choice));
        }
        if let Some(thinking) = &options.thinking {
            extra.insert("thinking".to_owned(), to_json_value(thinking));
        }
        if let Some(project) = &options.project {
            extra.insert("project".to_owned(), serde_json::Value::String(project.clone()));
        }
        if let Some(location) = &options.location {
            extra.insert("location".to_owned(), serde_json::Value::String(location.clone()));
        }
        Self {
            base: options.base,
            extra,
            extension: None,
        }
    }
}

impl GoogleVertexOptions {
    pub fn from_api_options(options: Option<ApiStreamOptions>) -> Self {
        let Some(opts) = options else {
            return Self::default();
        };
        let tool_choice = opts
            .extra
            .get("toolChoice")
            .cloned()
            .and_then(|v| serde_json::from_value::<GoogleToolChoice>(v).ok());
        let thinking = opts
            .extra
            .get("thinking")
            .cloned()
            .and_then(|v| serde_json::from_value::<GoogleThinkingOptions>(v).ok());
        let project = opts
            .extra
            .get("project")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let location = opts
            .extra
            .get("location")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Self {
            base: opts.base,
            tool_choice,
            thinking,
            project,
            location,
        }
    }
}

/// Resolved client/HTTP configuration for a Vertex request, mirroring the
/// parameters TS passes to `new GoogleGenAI(...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexRequestConfig {
    pub vertexai: bool,
    pub api_key: Option<String>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub api_version: String,
    pub base_url: Option<String>,
    pub base_url_resource_scope: Option<&'static str>,
    pub key_filename: Option<String>,
    pub headers: IndexMap<String, String>,
}

fn is_placeholder_api_key(api_key: &str) -> bool {
    let bytes = api_key.as_bytes();
    bytes.len() >= 3
        && bytes[0] == b'<'
        && bytes[bytes.len() - 1] == b'>'
        && !bytes[1..bytes.len() - 1].iter().any(|&b| b == b'<' || b == b'>')
}

fn resolve_api_key(options: &GoogleVertexOptions) -> Option<String> {
    let api_key = options.base.api_key.as_deref().map(str::trim)?;
    if api_key.is_empty() || api_key == GCP_VERTEX_CREDENTIALS_MARKER || is_placeholder_api_key(api_key) {
        return None;
    }
    Some(api_key.to_owned())
}

fn resolve_project(options: &GoogleVertexOptions) -> Result<String> {
    options
        .project
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_PROJECT", options.base.env.as_ref()))
        .or_else(|| get_provider_env_value("GCLOUD_PROJECT", options.base.env.as_ref()))
        .ok_or_else(|| {
            Error::msg(
                "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options.",
            )
        })
}

fn resolve_location(options: &GoogleVertexOptions) -> Result<String> {
    options
        .location
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_LOCATION", options.base.env.as_ref()))
        .ok_or_else(|| {
            Error::msg("Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options.")
        })
}

fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_owned())
}

fn api_version_segment_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^v\d+(?:beta\d*)?$").ok()).as_ref()
}

fn api_version_inline_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)v\d+(?:beta\d*)?(?:/|$)").ok())
        .as_ref()
}

fn base_url_includes_api_version(base_url: &str) -> bool {
    if let Ok(parsed) = Url::parse(base_url)
        && let Some(re) = api_version_segment_regex()
    {
        return parsed.path().split('/').any(|part| re.is_match(part));
    }
    api_version_inline_regex().is_some_and(|re| re.is_match(base_url))
}

fn build_google_auth_key_filename(env: Option<&ProviderEnv>) -> Option<String> {
    get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env)
}

pub fn resolve_vertex_request_config(model: &Model, options: &GoogleVertexOptions) -> Result<VertexRequestConfig> {
    let api_key = resolve_api_key(options);
    let custom_base_url = resolve_custom_base_url(&model.base_url);
    let api_version = match custom_base_url.as_deref() {
        Some(url) if base_url_includes_api_version(url) => String::new(),
        _ => API_VERSION.to_owned(),
    };
    let base_url_resource_scope = custom_base_url.as_ref().map(|_| RESOURCE_SCOPE_COLLECTION);
    let headers = build_google_headers(model.headers.as_ref(), options.base.headers.as_ref()).unwrap_or_default();

    if let Some(key) = api_key {
        Ok(VertexRequestConfig {
            vertexai: true,
            api_key: Some(key),
            project: None,
            location: None,
            api_version,
            base_url: custom_base_url,
            base_url_resource_scope,
            key_filename: None,
            headers,
        })
    } else {
        let project = resolve_project(options)?;
        let location = resolve_location(options)?;
        let key_filename = build_google_auth_key_filename(options.base.env.as_ref());
        Ok(VertexRequestConfig {
            vertexai: true,
            api_key: None,
            project: Some(project),
            location: Some(location),
            api_version,
            base_url: custom_base_url,
            base_url_resource_scope,
            key_filename,
            headers,
        })
    }
}

fn build_vertex_stream_url(cfg: &VertexRequestConfig, wire_model_id: &str) -> Result<String> {
    let model_path = format_google_model_path(wire_model_id, true)?;
    let root = if let Some(custom) = &cfg.base_url {
        custom.trim_end_matches('/').to_owned()
    } else if cfg.api_key.is_some() && cfg.project.is_none() {
        "https://aiplatform.googleapis.com".to_owned()
    } else if let Some(location) = cfg.location.as_deref() {
        match location {
            "global" => "https://aiplatform.googleapis.com".to_owned(),
            "us" | "eu" => format!("https://aiplatform.{location}.rep.googleapis.com"),
            other => format!("https://{other}-aiplatform.googleapis.com"),
        }
    } else {
        "https://aiplatform.googleapis.com".to_owned()
    };

    let mut url = root;
    if !cfg.api_version.is_empty() {
        url.push('/');
        url.push_str(&cfg.api_version);
    }

    let is_collection_scope = cfg.base_url_resource_scope == Some(RESOURCE_SCOPE_COLLECTION);
    if !is_collection_scope
        && !model_path.starts_with("projects/")
        && let (Some(project), Some(location)) = (&cfg.project, &cfg.location)
    {
        url.push_str(&format!("/projects/{project}/locations/{location}"));
    }

    url.push('/');
    url.push_str(&model_path);
    url.push_str(":streamGenerateContent?alt=sse");
    Ok(url)
}

async fn resolve_adc_token(key_filename: Option<&str>) -> Result<String> {
    if let Some(mock_token) = testing::current_mock_adc_token() {
        return Ok(mock_token);
    }

    if let Some(path) = key_filename {
        let sa = gcp_auth::CustomServiceAccount::from_file(path).map_err(|err| {
            Error::msg(format!(
                "Failed to load Google service account credentials from {path}: {err}"
            ))
        })?;
        let token = sa.token(&[VERTEX_CLOUD_SCOPE]).await.map_err(|err| {
            Error::msg(format!(
                "Failed to obtain Google Vertex access token from service account: {err}"
            ))
        })?;
        return Ok(token.as_str().to_owned());
    }

    let provider = gcp_auth::provider().await.map_err(|err| {
        Error::msg(format!(
            "Failed to initialize Google Application Default Credentials: {err}"
        ))
    })?;
    let token = provider
        .token(&[VERTEX_CLOUD_SCOPE])
        .await
        .map_err(|err| Error::msg(format!("Failed to obtain Google Vertex access token from ADC: {err}")))?;
    Ok(token.as_str().to_owned())
}

async fn build_vertex_headers(cfg: &VertexRequestConfig) -> Result<Headers> {
    let mut headers = Headers::new();
    for (name, value) in &cfg.headers {
        headers.set(name, value);
    }
    if headers.get("content-type").is_none() {
        headers.set("content-type", "application/json");
    }
    if headers.get("accept").is_none() {
        headers.set("accept", "text/event-stream");
    }

    if let Some(api_key) = &cfg.api_key {
        if api_key.starts_with("auth_tokens/") {
            return Err(Error::msg("Ephemeral tokens are only supported by the live API."));
        }
        if headers.get("x-goog-api-key").is_none() {
            headers.set("x-goog-api-key", api_key);
        }
    } else if headers.get("authorization").is_none() {
        let token = resolve_adc_token(cfg.key_filename.as_deref()).await?;
        headers.set("authorization", &format!("Bearer {token}"));
    }

    Ok(headers)
}

pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<GoogleVertexOptions>,
) -> AssistantMessageEventStream {
    let normalized_context = collapse_system_messages(context);
    let model_owned = model.clone();
    let opts = options.unwrap_or_default();
    let signal = opts.base.signal.clone();

    spawn_google_stream(model_owned.clone(), signal, move || {
        Box::pin(async move {
            if opts.base.fetch.is_some() {
                return Err(Error::msg("Custom fetch is not supported by the Google Vertex adapter"));
            }
            let cfg = resolve_vertex_request_config(&model_owned, &opts)?;
            let mut params = build_google_generate_params(
                &model_owned,
                &normalized_context,
                GoogleParamsInput {
                    temperature: opts.base.temperature,
                    max_tokens: opts.base.max_tokens,
                    tool_choice: opts.tool_choice,
                    thinking: opts.thinking.as_ref(),
                    signal: opts.base.signal.as_ref(),
                },
            )?;
            params = apply_on_payload(params, opts.base.on_payload.as_ref(), &model_owned).await?;

            let (wire_model_id, wire_body) = project_google_params_to_wire_body(&params, true)?;
            let url = build_vertex_stream_url(&cfg, &wire_model_id)?;
            let headers = build_vertex_headers(&cfg).await?;
            let retry_signal = opts.base.signal.clone();

            Ok(GoogleStreamRequest {
                url,
                headers,
                wire_body,
                signal: opts.base.signal,
                retry_options: ProviderRetryOptions {
                    max_retries: opts.base.max_retries,
                    max_retry_delay_ms: opts.base.max_retry_delay_ms,
                    signal: retry_signal,
                },
                on_provider_stream_event: opts.base.on_provider_stream_event,
                missing_finish_reason_message: "Google Vertex stream ended without a finish reason",
            })
        }) as BoxFuture<Result<GoogleStreamRequest>>
    })
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(GoogleVertexOptions::from_api_options(options)))
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let base = build_base_options(model, context, options.as_ref(), None);
    let thinking = match resolve_simple_google_thinking(
        model,
        options.as_ref().and_then(|o| o.reasoning),
        options.as_ref().and_then(|o| o.thinking_budgets.as_ref()),
        None,
    ) {
        Ok(thinking) => thinking,
        Err(err) => return lazy_stream(model, async move { Err(err) }),
    };

    stream_with_options(
        model,
        context,
        Some(GoogleVertexOptions {
            base,
            tool_choice: options.as_ref().and_then(|o| o.tool_choice.map(Into::into)),
            thinking: Some(thinking),
            project: None,
            location: None,
        }),
    )
}

pub fn provider_streams() -> ProviderStreams {
    ProviderStreams {
        stream: Arc::new(|model, context, options| stream(model, context, options)),
        stream_simple: Arc::new(|model, context, options| stream_simple(model, context, options)),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

pub mod testing {
    use std::cell::RefCell;

    thread_local! {
        static MOCK_ADC_TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    /// Thread-local RAII guard overriding Vertex ADC bearer token lookup for tests.
    #[must_use = "the ADC token mock is removed when the guard drops"]
    pub struct AdcTokenGuard {
        prev: Option<String>,
    }

    impl Drop for AdcTokenGuard {
        fn drop(&mut self) {
            let prev = self.prev.take();
            MOCK_ADC_TOKEN.with(|slot| {
                *slot.borrow_mut() = prev;
            });
        }
    }

    pub fn mock_adc_token(token: impl Into<String>) -> AdcTokenGuard {
        let next = Some(token.into());
        let prev = MOCK_ADC_TOKEN.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next));
        AdcTokenGuard { prev }
    }

    pub(crate) fn current_mock_adc_token() -> Option<String> {
        MOCK_ADC_TOKEN.with(|slot| slot.borrow().clone())
    }
}

//! Port of packages/ai/src/api/google-generative-ai.ts

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use pi_js::fetch::Headers;
use pi_js::{BoxFuture, Error, Result};

use crate::api::google_shared::{
    GoogleParamsInput, GoogleStreamRequest, GoogleThinkingOptions, GoogleToolChoice, apply_on_payload,
    build_google_generate_params, build_google_headers, format_google_model_path, project_google_params_to_wire_body,
    resolve_simple_google_thinking, spawn_google_stream, to_json_value,
};
use crate::api::lazy::lazy_stream;
use crate::api::simple_options::build_base_options;
use crate::types::{
    ApiStreamOptions, JsonObject, Model, ProviderHeaders, ProviderStreams, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::provider_retry::ProviderRetryOptions;
use crate::utils::transcript::collapse_system_messages;

const DEFAULT_GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_GOOGLE_API_VERSION: &str = "v1beta";

#[derive(Clone, Default, Debug)]
pub struct GoogleOptions {
    pub base: StreamOptions,
    pub tool_choice: Option<GoogleToolChoice>,
    pub thinking: Option<GoogleThinkingOptions>,
}

impl Deref for GoogleOptions {
    type Target = StreamOptions;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for GoogleOptions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl From<StreamOptions> for GoogleOptions {
    fn from(base: StreamOptions) -> Self {
        Self {
            base,
            ..Default::default()
        }
    }
}

impl From<GoogleOptions> for ApiStreamOptions {
    fn from(options: GoogleOptions) -> Self {
        let mut extra = JsonObject::new();
        if let Some(tool_choice) = options.tool_choice {
            extra.insert("toolChoice".to_owned(), to_json_value(&tool_choice));
        }
        if let Some(thinking) = &options.thinking {
            extra.insert("thinking".to_owned(), to_json_value(thinking));
        }
        Self {
            base: options.base,
            extra,
            extension: None,
        }
    }
}

impl GoogleOptions {
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
        Self {
            base: opts.base,
            tool_choice,
            thinking,
        }
    }
}

fn build_google_ai_url(model: &Model, wire_model_id: &str) -> Result<String> {
    let model_path = format_google_model_path(wire_model_id, false)?;
    let custom_base = model.base_url.trim();
    if custom_base.is_empty() {
        Ok(format!(
            "{DEFAULT_GOOGLE_BASE_URL}/{DEFAULT_GOOGLE_API_VERSION}/{model_path}:streamGenerateContent?alt=sse"
        ))
    } else {
        // When `model.base_url` is set, TS passes `apiVersion: ""` because base_url already includes version path.
        let trimmed = custom_base.trim_end_matches('/');
        Ok(format!("{trimmed}/{model_path}:streamGenerateContent?alt=sse"))
    }
}

fn build_google_ai_headers(
    model: &Model,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&crate::utils::identity::ClientIdentity>,
) -> Result<Headers> {
    if api_key.starts_with("auth_tokens/") {
        return Err(Error::msg("Ephemeral tokens are only supported by the live API."));
    }
    let mut headers = Headers::new();
    if let Some(merged) = build_google_headers(model, options_headers, identity) {
        for (name, value) in merged {
            headers.set(&name, &value);
        }
    }
    if headers.get("content-type").is_none() {
        headers.set("content-type", "application/json");
    }
    if headers.get("accept").is_none() {
        headers.set("accept", "text/event-stream");
    }
    if headers.get("x-goog-api-key").is_none() {
        headers.set("x-goog-api-key", api_key);
    }
    Ok(headers)
}

pub fn stream_with_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<GoogleOptions>,
) -> AssistantMessageEventStream {
    let normalized_context = collapse_system_messages(context);
    let model_owned = model.clone();
    let opts = options.unwrap_or_default();
    let signal = opts.base.signal.clone();

    spawn_google_stream(model_owned.clone(), signal, move || {
        Box::pin(async move {
            if opts.base.fetch.is_some() {
                return Err(Error::msg(
                    "Custom fetch is not supported by the Google Generative AI adapter",
                ));
            }
            let api_key = opts
                .base
                .api_key
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::msg(format!("No API key for provider: {}", model_owned.provider)))?
                .to_owned();

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

            let (wire_model_id, wire_body) = project_google_params_to_wire_body(&params, false)?;
            let url = build_google_ai_url(&model_owned, &wire_model_id)?;
            let headers = build_google_ai_headers(
                &model_owned,
                &api_key,
                opts.base.headers.as_ref(),
                opts.base.identity.as_ref(),
            )?;

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
                missing_finish_reason_message: "Google stream ended without a finish reason",
            })
        }) as BoxFuture<Result<GoogleStreamRequest>>
    })
}

pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    stream_with_options(model, context, Some(GoogleOptions::from_api_options(options)))
}

pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let Some(api_key) = options
        .as_ref()
        .and_then(|o| o.base.api_key.as_deref())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        let provider = model.provider.clone();
        return lazy_stream(model, async move {
            Err(Error::msg(format!("No API key for provider: {provider}")))
        });
    };

    let base = build_base_options(model, context, options.as_ref(), Some(&api_key));
    let thinking = match resolve_simple_google_thinking(
        model,
        options.as_ref().and_then(|o| o.reasoning),
        options.as_ref().and_then(|o| o.thinking_budgets.as_ref()),
        Some(512),
    ) {
        Ok(thinking) => thinking,
        Err(err) => return lazy_stream(model, async move { Err(err) }),
    };

    stream_with_options(
        model,
        context,
        Some(GoogleOptions {
            base,
            tool_choice: options.as_ref().and_then(|o| o.tool_choice.map(Into::into)),
            thinking: Some(thinking),
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

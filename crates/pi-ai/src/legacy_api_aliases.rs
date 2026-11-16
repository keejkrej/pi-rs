//! Port of packages/ai/src/legacy-api-aliases.ts

use std::sync::LazyLock;

use crate::api::anthropic_messages_lazy::anthropic_messages_api;
use crate::api::azure_openai_responses_lazy::azure_openai_responses_api;
use crate::api::openai_codex_responses_lazy::openai_codex_responses_api;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::api::openai_responses_lazy::openai_responses_api;
use crate::types::{ApiStreamOptions, Model, ProviderStreams, SimpleStreamOptions, TranscriptContext};
use crate::utils::event_stream::AssistantMessageEventStream;

static ANTHROPIC_STREAMS: LazyLock<ProviderStreams> = LazyLock::new(anthropic_messages_api);
static AZURE_OPENAI_STREAMS: LazyLock<ProviderStreams> = LazyLock::new(azure_openai_responses_api);
static OPENAI_CODEX_STREAMS: LazyLock<ProviderStreams> = LazyLock::new(openai_codex_responses_api);
static OPENAI_COMPLETIONS_STREAMS: LazyLock<ProviderStreams> = LazyLock::new(openai_completions_api);
static OPENAI_RESPONSES_STREAMS: LazyLock<ProviderStreams> = LazyLock::new(openai_responses_api);

#[deprecated(note = "Use crate::api::anthropic_messages::stream or anthropic_messages_api().stream")]
pub fn stream_anthropic(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    (ANTHROPIC_STREAMS.stream)(model, context, options)
}

#[deprecated(note = "Use crate::api::anthropic_messages::stream_simple or anthropic_messages_api().stream_simple")]
pub fn stream_simple_anthropic(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    (ANTHROPIC_STREAMS.stream_simple)(model, context, options)
}

#[deprecated(note = "Use crate::api::azure_openai_responses::stream or azure_openai_responses_api().stream")]
pub fn stream_azure_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    (AZURE_OPENAI_STREAMS.stream)(model, context, options)
}

#[deprecated(
    note = "Use crate::api::azure_openai_responses::stream_simple or azure_openai_responses_api().stream_simple"
)]
pub fn stream_simple_azure_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    (AZURE_OPENAI_STREAMS.stream_simple)(model, context, options)
}

#[deprecated(note = "Use crate::api::openai_codex_responses::stream or openai_codex_responses_api().stream")]
pub fn stream_openai_codex_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_CODEX_STREAMS.stream)(model, context, options)
}

#[deprecated(
    note = "Use crate::api::openai_codex_responses::stream_simple or openai_codex_responses_api().stream_simple"
)]
pub fn stream_simple_openai_codex_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_CODEX_STREAMS.stream_simple)(model, context, options)
}

#[deprecated(note = "Use crate::api::openai_completions::stream or openai_completions_api().stream")]
pub fn stream_openai_completions(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_COMPLETIONS_STREAMS.stream)(model, context, options)
}

#[deprecated(note = "Use crate::api::openai_completions::stream_simple or openai_completions_api().stream_simple")]
pub fn stream_simple_openai_completions(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_COMPLETIONS_STREAMS.stream_simple)(model, context, options)
}

#[deprecated(note = "Use crate::api::openai_responses::stream or openai_responses_api().stream")]
pub fn stream_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<ApiStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_RESPONSES_STREAMS.stream)(model, context, options)
}

#[deprecated(note = "Use crate::api::openai_responses::stream_simple or openai_responses_api().stream_simple")]
pub fn stream_simple_openai_responses(
    model: &Model,
    context: &TranscriptContext,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    (OPENAI_RESPONSES_STREAMS.stream_simple)(model, context, options)
}

// WIRE(@mcp-w2): add deprecated wrappers for `stream_google`, `stream_simple_google`,
// `stream_google_vertex`, `stream_simple_google_vertex`, `stream_mistral`, `stream_simple_mistral`.

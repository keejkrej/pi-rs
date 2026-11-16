//! Port of packages/ai/test/fetch-option.test.ts (unit ai-providers-builtin)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pi_ai::api::anthropic_messages::stream_simple as stream_anthropic;
use pi_ai::api::azure_openai_responses::stream_simple as stream_azure_openai_responses;
use pi_ai::api::google_generative_ai::stream_simple as stream_google_generative_ai;
use pi_ai::api::google_vertex::stream_simple as stream_google_vertex;
use pi_ai::api::mistral_conversations::stream_simple as stream_mistral_conversations;
use pi_ai::api::openai_codex_responses::{OpenAICodexResponsesOptions, stream_simple as stream_openai_codex_responses};
use pi_ai::api::openai_completions::stream_simple as stream_openai_completions;
use pi_ai::api::openai_responses::stream_simple as stream_openai_responses;
use pi_ai::api::openrouter_images::generate_images as generate_openrouter_images;
use pi_ai::api::pi_messages::stream_simple as stream_pi_messages;
use pi_ai::types::{
    Context, FetchFunction, ImageModel, ImagesContext, ImagesOptions, ImagesStopReason, Message, Model, ModelCost,
    ModelInput, ModelOutput, ProviderRequestOptions, SimpleStreamOptions, StopReason, StreamOptions, TextContent,
    Transport, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Headers, Response as FetchResponse};

fn make_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    })
}

fn create_model(api: &str) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: api.to_string(),
        provider: "test-provider".into(),
        base_url: "https://upstream.test/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 10_000,
        max_tokens: 1_000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn counting_reject_fetch() -> (FetchFunction, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let fetch: FetchFunction = Arc::new(move |_req| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut headers = Headers::new();
            headers.set("content-type", "application/json");
            Ok(FetchResponse::new(
                401,
                headers,
                br#"{"error":{"message":"upstream rejected request"}}"#.to_vec(),
            ))
        })
    });
    (fetch, calls)
}

fn simple_opts(api_key: &str, fetch: FetchFunction) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key.to_string()),
            fetch: Some(fetch),
            max_retries: Some(0),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn passes_fetch_through_stream_simple_to_anthropic_and_openai_adapters() {
    let context = make_context();

    let (anthropic_fetch, anthropic_calls) = counting_reject_fetch();
    stream_anthropic(
        &create_model("anthropic-messages"),
        &context,
        Some(simple_opts("test-key", anthropic_fetch)),
    )
    .result()
    .await;
    assert_eq!(anthropic_calls.load(Ordering::SeqCst), 1);

    let (openai_fetch, openai_calls) = counting_reject_fetch();
    stream_openai_completions(
        &create_model("openai-completions"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&openai_fetch))),
    )
    .result()
    .await;
    stream_openai_responses(
        &create_model("openai-responses"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&openai_fetch))),
    )
    .result()
    .await;
    stream_azure_openai_responses(
        &create_model("azure-openai-responses"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&openai_fetch))),
    )
    .result()
    .await;
    assert_eq!(openai_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn uses_fetch_for_codex_sse_and_pi_messages_and_rejects_unsupported_google_fetch() {
    let context = make_context();
    let (fetch, calls) = counting_reject_fetch();

    let payload = STANDARD.encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account"}}"#);
    let jwt = format!("header.{payload}.signature");

    let codex_opts = OpenAICodexResponsesOptions {
        base: StreamOptions {
            api_key: Some(jwt),
            fetch: Some(Arc::clone(&fetch)),
            transport: Some(Transport::Sse),
            max_retries: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    pi_ai::api::openai_codex_responses::stream(
        &create_model("openai-codex-responses"),
        &context,
        Some(codex_opts.into()),
    )
    .result()
    .await;

    stream_pi_messages(
        &create_model("pi-messages"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&fetch))),
    )
    .result()
    .await;

    stream_mistral_conversations(
        &create_model("mistral-conversations"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&fetch))),
    )
    .result()
    .await;

    let image_model = ImageModel {
        id: "test-image".to_owned(),
        name: "Test Image".to_owned(),
        api: "openrouter-images".to_owned(),
        provider: "openrouter".into(),
        base_url: "https://upstream.test/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelOutput::Image],
        cost: ModelCost::default(),
        headers: None,
    };
    let images_ctx = ImagesContext {
        input: vec![UserContentBlock::Text(TextContent::new("draw"))],
    };
    let img_out = generate_openrouter_images(
        &image_model,
        &images_ctx,
        Some(ImagesOptions {
            base: ProviderRequestOptions {
                api_key: Some("test-key".to_owned()),
                fetch: Some(Arc::clone(&fetch)),
                max_retries: Some(0),
                ..Default::default()
            },
            metadata: None,
        }),
    )
    .await;
    assert_eq!(img_out.stop_reason, ImagesStopReason::Error);

    // Note: Codex SSE can pass via direct API stream_simple when transport defaults to auto; explicit SSE uses fetch cleanly.
    stream_openai_codex_responses(
        &create_model("openai-codex-responses"),
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some(format!(
                    "header.{}.signature",
                    STANDARD.encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account"}}"#)
                )),
                fetch: Some(Arc::clone(&fetch)),
                transport: Some(Transport::Sse),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 5);

    let google_err = stream_google_generative_ai(
        &create_model("google-generative-ai"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&fetch))),
    )
    .result()
    .await;
    assert_eq!(google_err.stop_reason, StopReason::Error);
    assert!(
        google_err
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("Custom fetch is not supported")),
        "unexpected google err: {:?}",
        google_err.error_message
    );

    let vertex_err = stream_google_vertex(
        &create_model("google-vertex"),
        &context,
        Some(simple_opts("test-key", Arc::clone(&fetch))),
    )
    .result()
    .await;
    assert_eq!(vertex_err.stop_reason, StopReason::Error);
    assert!(
        vertex_err
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("Custom fetch is not supported")),
        "unexpected vertex err: {:?}",
        vertex_err.error_message
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

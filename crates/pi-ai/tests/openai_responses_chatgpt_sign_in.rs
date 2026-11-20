//! Port of packages/ai/test/openai-responses-chatgpt-sign-in.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, Context, Message, Model, ModelCompat, ModelCost, ModelInput,
    OpenAIResponsesCompat, StreamOptions, TextContent, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use serde_json::{Value, json};

fn base_model() -> Model {
    Model {
        r#type: None,
        id: "gpt-5-mini".to_owned(),
        name: "GPT-5 Mini".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        base_url: "https://api.openai.com/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 400_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("hi"))]),
            timestamp: 0,
        })],
        tools: Some(Vec::new()),
    })
}

async fn capture_payload(api_key: &str, request_model: &Model) -> Value {
    let payload = Arc::new(Mutex::new(None));
    let p_c = payload.clone();

    stream_openai_responses(
        request_model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some(api_key.to_owned()),
                max_tokens: Some(1000),
                temperature: Some(0.5),
                cache_retention: Some(CacheRetention::Long),
                on_payload: Some(Arc::new(move |params, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(params);
                        Ok(None)
                    })
                })),
                fetch: Some(Arc::new(|_| {
                    Box::pin(async { Ok(Response::null_body(500, Headers::new())) })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    payload
        .lock()
        .unwrap()
        .take()
        .expect("Request payload was not captured")
}

#[tokio::test]
async fn omits_request_fields_that_token_sharing_rejects() {
    let payload = capture_payload("chatgpt-access-token", &base_model()).await;
    assert!(payload.get("max_output_tokens").is_none());
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn omits_prompt_cache_options_on_models_with_explicit_prompt_cache_mode() {
    let mut explicit_cache_model = base_model();
    explicit_cache_model.compat = Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        supports_explicit_prompt_cache_mode: Some(true),
        ..Default::default()
    }));

    let sign_in_payload = capture_payload("chatgpt-access-token", &explicit_cache_model).await;
    let api_key_payload = capture_payload("sk-proj-test", &explicit_cache_model).await;

    assert!(sign_in_payload.get("prompt_cache_options").is_none());
    assert_eq!(
        api_key_payload.get("prompt_cache_options"),
        Some(&json!({ "ttl": "30m" }))
    );
}

#[tokio::test]
async fn keeps_those_fields_for_api_keys_and_other_openai_compatible_endpoints() {
    let cases = [
        ("sk-proj-test", base_model()),
        (
            "gateway-key",
            Model {
                base_url: "https://gateway.example.com/v1".to_owned(),
                ..base_model()
            },
        ),
    ];

    for (api_key, request_model) in cases {
        let payload = capture_payload(api_key, &request_model).await;
        assert_eq!(payload.get("max_output_tokens"), Some(&json!(1000)));
        assert_eq!(payload.get("temperature"), Some(&json!(0.5)));
        assert_eq!(payload.get("prompt_cache_retention"), Some(&json!("24h")));
    }
}

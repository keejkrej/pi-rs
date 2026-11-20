//! Port of packages/ai/test/azure-openai-tool-choice.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::azure_openai_responses::{stream, stream_simple};
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, Context, Message, Model, ModelCost, ModelInput, SimpleStreamOptions, StreamOptions, Tool,
    ToolChoice, UserMessage, UserMessageContent,
};
use pi_js::Error;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "test-deployment".to_owned(),
        name: "Test Deployment".to_owned(),
        api: "azure-openai-responses".to_owned(),
        provider: "azure".to_owned(),
        base_url: "http://127.0.0.1:9/openai/v1".to_owned(),
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
        sampling_params_by_thinking_level: None,
    }
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Summarize this".to_owned()),
            timestamp: 1,
        })],
        tools: Some(vec![Tool {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: t::object([("path", t::string())]),
            constrained_sampling: None,
        }]),
    })
}

#[tokio::test]
async fn forwards_provider_specific_tool_choice_while_preserving_tool_definitions() {
    let payload = Arc::new(Mutex::new(None));
    let p_c = payload.clone();

    stream(
        &test_model(),
        &sample_context(),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_owned()),
                    on_payload: Some(Arc::new(move |val, _| {
                        let p = p_c.clone();
                        Box::pin(async move {
                            *p.lock().unwrap() = Some(val);
                            Err(Error::msg("payload captured"))
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("toolChoice", json!("required")),
        ),
    )
    .result()
    .await;

    let captured = payload.lock().unwrap().take().unwrap();
    assert_eq!(captured.get("tool_choice"), Some(&json!("required")));
    assert_eq!(captured.get("tools").and_then(Value::as_array).map(Vec::len), Some(1));
}

#[tokio::test]
async fn forwards_provider_neutral_tool_choice_from_simple_options() {
    let payload = Arc::new(Mutex::new(None));
    let p_c = payload.clone();

    stream_simple(
        &test_model(),
        &sample_context(),
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                on_payload: Some(Arc::new(move |val, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(val);
                        Err(Error::msg("payload captured"))
                    })
                })),
                ..Default::default()
            },
            tool_choice: Some(ToolChoice::None),
            ..Default::default()
        }),
    )
    .result()
    .await;

    let captured = payload.lock().unwrap().take().unwrap();
    assert_eq!(captured.get("tool_choice"), Some(&json!("none")));
    assert_eq!(captured.get("tools").and_then(Value::as_array).map(Vec::len), Some(1));
}

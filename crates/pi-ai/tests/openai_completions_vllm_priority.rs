//! Port of packages/ai/test/openai-completions-vllm-priority.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, Context, Message, Model, ModelCompat, OpenAICompletionsCompat, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn create_model(compat: Option<OpenAICompletionsCompat>) -> Model {
    let mut base = get_model("openai", "gpt-4o-mini").unwrap();
    base.compat = compat.map(ModelCompat::OpenAICompletions);
    base.api = "openai-completions".to_owned();
    base
}

fn ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        })
    )
}

async fn capture_request(model: &Model) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let cap_c = captured.clone();

    let ctx = normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    stream_openai_completions(
        model,
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                on_payload: Some(Arc::new(move |params, _| {
                    let c = cap_c.clone();
                    Box::pin(async move {
                        *c.lock().unwrap() = Some(params);
                        Ok(None)
                    })
                })),
                fetch: Some(Arc::new(|_| {
                    Box::pin(async {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            ok_sse(),
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    captured.lock().unwrap().take().unwrap()
}

#[tokio::test]
async fn sends_compat_vllm_priority_as_top_level_priority_request_field() {
    let model = create_model(Some(OpenAICompletionsCompat {
        vllm_priority: Some(10),
        ..Default::default()
    }));
    let payload = capture_request(&model).await;
    assert_eq!(payload.get("priority"), Some(&json!(10)));
}

#[tokio::test]
async fn omits_priority_when_vllm_priority_is_not_set() {
    let model = create_model(None);
    let payload = capture_request(&model).await;
    assert!(payload.get("priority").is_none());
}

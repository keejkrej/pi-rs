//! Port of packages/ai/test/openai-completions-provider-stream-event.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::compat::complete_simple;
use pi_ai::types::{
    AssistantContentBlock, Context, Message, Model, ModelCost, ModelInput, SimpleStreamOptions, StreamOptions,
    TextContent, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn openrouter_model() -> Model {
    Model {
        r#type: None,
        id: "openrouter/auto".to_owned(),
        name: "OpenRouter Auto".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[tokio::test]
async fn exposes_provider_chunks_including_openrouter_metadata() {
    let first_chunk = json!({
        "id": "chatcmpl-1",
        "model": "anthropic/claude-sonnet-4.6",
        "choices": [{ "index": 0, "delta": { "content": "hello" } }],
    });
    let final_chunk = json!({
        "id": "chatcmpl-1",
        "model": "anthropic/claude-sonnet-4.6",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12,
            "cost": 0.0012,
            "is_byok": false,
        },
        "openrouter_metadata": { "strategy": "direct", "region": "iad" },
    });

    let sse = format!("data: {first_chunk}\n\ndata: {final_chunk}\n\ndata: [DONE]\n\n");
    let seen_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let se_c = seen_events.clone();

    let message = complete_simple(
        &openrouter_model(),
        &Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            })],
            tools: None,
        },
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                on_provider_stream_event: Some(Arc::new(move |data, _| {
                    let se = se_c.clone();
                    let val = data.clone();
                    Box::pin(async move {
                        se.lock().unwrap().push(val);
                        Ok(())
                    })
                })),
                fetch: Some(Arc::new(move |_| {
                    let body = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream"), ("x-request-id", "req-1")]),
                            body,
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .await;

    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("hello"))]
    );
    assert_eq!(*seen_events.lock().unwrap(), vec![first_chunk, final_chunk]);
}

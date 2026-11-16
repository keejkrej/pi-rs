//! Port of packages/ai/test/anthropic-cache-write-1h-cost.test.ts

use pi_ai::api::anthropic_messages::{AnthropicClient, AnthropicOptions, stream_with_options};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{Context, Message, UserMessage, UserMessageContent};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::{Value, json};

fn create_sse_response(events: &[(&str, Value)]) -> Response {
    let body = events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {}\n", pi_js::json::stringify(data)))
        .collect::<Vec<_>>()
        .join("\n");
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

fn create_fake_anthropic_client(resp: Response) -> AnthropicClient {
    let r = std::sync::Mutex::new(Some(resp));
    AnthropicClient::from_handler(move |_, _| {
        let taken = r.lock().unwrap().take().expect("fake response already consumed");
        async move { Ok(taken) }
    })
}

fn events_with_cache_creation(cache_creation: Option<Value>) -> Vec<(&'static str, Value)> {
    let mut start_usage = json!({
        "input_tokens": 100,
        "output_tokens": 0,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 1_000_000
    });
    if let Some(cc) = cache_creation {
        start_usage
            .as_object_mut()
            .unwrap()
            .insert("cache_creation".to_owned(), cc);
    }
    vec![
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": { "id": "msg_test", "usage": start_usage }
            }),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            }),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hi" }
            }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 1_000_000
                }
            }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

#[tokio::test]
async fn prices_the_1h_portion_at_2x_input_and_the_rest_at_the_5m_rate() {
    let model = get_model("anthropic", "claude-opus-4-8").unwrap();
    let events = events_with_cache_creation(Some(json!({
        "ephemeral_5m_input_tokens": 600_000,
        "ephemeral_1h_input_tokens": 400_000
    })));
    let refs: Vec<(&str, Value)> = events.iter().map(|(k, v)| (*k, v.clone())).collect();
    let response = create_sse_response(&refs);

    let result = stream_with_options(
        &model,
        &sample_context(),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write_1h, Some(400_000));
    assert!((result.usage.cost.cache_write - 7.75).abs() < 1e-10);
}

#[tokio::test]
async fn prices_1h_cache_writes_reported_only_in_message_delta() {
    let model = get_model("vercel-ai-gateway", "anthropic/claude-haiku-4.5").unwrap();
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": { "id": "msg_test", "usage": { "input_tokens": 0, "output_tokens": 0 } }
            }),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 4,
                    "cache_creation_input_tokens": 6535,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 0,
                        "ephemeral_1h_input_tokens": 6535
                    }
                }
            }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]);

    let result = stream_with_options(
        &model,
        &sample_context(),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.usage.cache_write, 6535);
    assert_eq!(result.usage.cache_write_1h, Some(6535));
    let expected = (6535.0 * model.cost.input * 2.0) / 1_000_000.0;
    assert!((result.usage.cost.cache_write - expected).abs() < 1e-10);
}

#[tokio::test]
async fn falls_back_to_the_5m_rate_when_no_breakdown_is_reported() {
    let model = get_model("anthropic", "claude-opus-4-8").unwrap();
    let events = events_with_cache_creation(None);
    let refs: Vec<(&str, Value)> = events.iter().map(|(k, v)| (*k, v.clone())).collect();
    let response = create_sse_response(&refs);

    let result = stream_with_options(
        &model,
        &sample_context(),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write_1h.unwrap_or(0), 0);
    assert!((result.usage.cost.cache_write - 6.25).abs() < 1e-10);
}

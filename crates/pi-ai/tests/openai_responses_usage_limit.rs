//! Port of packages/ai/test/openai-responses-usage-limit.test.ts

use std::sync::Arc;

use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{ApiStreamOptions, Context, Message, StopReason, StreamOptions, UserMessage, UserMessageContent};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use serde_json::json;

#[tokio::test]
async fn adds_chatgpt_usage_link_when_http_429_reports_subscription_sharing_usage_limit() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    let body = json!({
        "error": {
            "code": "subscription_sharing_usage_limit_exceeded",
            "message": "Usage limit reached.",
            "type": "rate_limit_error",
        }
    })
    .to_string();

    let result = stream_openai_responses(
        &model,
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("oauth-token".to_owned()),
                max_retries: Some(0),
                fetch: Some(Arc::new(move |_| {
                    let b = body.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            429,
                            Headers::from_pairs([("content-type", "application/json")]),
                            b,
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

    assert_eq!(result.stop_reason, StopReason::Error);
    let msg = result.error_message.as_deref().unwrap_or("");
    assert!(msg.contains("subscription_sharing_usage_limit_exceeded"), "{msg}");
    assert!(
        msg.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{msg}"
    );
}

#[tokio::test]
async fn adds_chatgpt_usage_link_when_sse_response_failed_reports_subscription_sharing_usage_limit() {
    let model = get_model("openai", "gpt-5.4").unwrap();
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    let sse = format!(
        "event: response.failed\ndata: {}\n\n",
        json!({
            "type": "response.failed",
            "response": {
                "id": "resp_1",
                "status": "failed",
                "error": {
                    "code": "subscription_sharing_usage_limit_exceeded",
                    "message": "Usage limit reached.",
                }
            }
        })
    );

    let result = stream_openai_responses(
        &model,
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("oauth-token".to_owned()),
                max_retries: Some(0),
                fetch: Some(Arc::new(move |_| {
                    let s = sse.clone();
                    Box::pin(async move {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            s,
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

    assert_eq!(result.stop_reason, StopReason::Error);
    let msg = result.error_message.as_deref().unwrap_or("");
    assert!(
        msg.contains("subscription_sharing_usage_limit_exceeded: Usage limit reached."),
        "{msg}"
    );
    assert!(
        msg.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"),
        "{msg}"
    );
}

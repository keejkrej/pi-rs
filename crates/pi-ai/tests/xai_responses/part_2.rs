//! Port of packages/ai/test/xai-responses.test.ts (unit ai-api-openai-responses)

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::normalize_context;
use pi_ai::providers::xai_models::xai_models;
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, Context, Message, Model, StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::identity::format_user_agent;
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

struct CapturedRequest {
    url: String,
    headers: Headers,
    body: Value,
}

async fn capture_request(model: &Model, context: &Context, opts: ApiStreamOptions) -> CapturedRequest {
    let captured_req = Arc::new(Mutex::new(None::<(String, Headers)>));
    let cr_c = captured_req.clone();
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let cb_c = captured_body.clone();

    let mut base = opts.base;
    base.fetch = Some(Arc::new(move |req: Request| {
        let cr = cr_c.clone();
        Box::pin(async move {
            *cr.lock().unwrap() = Some((req.url, req.headers));
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                "data: [DONE]\n\n",
            ))
        })
    }));
    base.on_payload = Some(Arc::new(move |payload, _| {
        let cb = cb_c.clone();
        Box::pin(async move {
            *cb.lock().unwrap() = Some(payload);
            Ok(None)
        })
    }));

    stream_openai_responses(
        model,
        &normalize_context(context),
        Some(ApiStreamOptions { base, ..opts }),
    )
    .result()
    .await;

    let (url, headers) = captured_req.lock().unwrap().take().unwrap();
    let body = captured_body.lock().unwrap().take().unwrap();
    CapturedRequest { url, headers, body }
}

#[tokio::test]
async fn uses_responses_with_bearer_auth_and_xai_compatible_request_fields() {
    let grok = xai_models().get("grok-4.5").expect("grok-4.5");
    let captured = capture_request(
        grok,
        &Context {
            system_prompt: Some("You are a careful coding assistant.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".to_owned()),
                session_id: Some("pi-session-123".to_owned()),
                cache_retention: Some(CacheRetention::Long),
                ..Default::default()
            },
            ..Default::default()
        }
        .with_extra("reasoningEffort", json!("medium")),
    )
    .await;

    let pi_ua = format_user_agent(None);
    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(
        captured.headers.get("authorization").as_deref(),
        Some("Bearer xai-test-token")
    );
    assert_eq!(captured.headers.get("user-agent").as_deref(), Some(pi_ua.as_str()));
    assert_eq!(captured.headers.get("session-id").as_deref(), Some("pi-session-123"));
    assert!(captured.headers.get("session_id").is_none());
    assert_eq!(captured.body["model"], json!("grok-4.5"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(captured.body["stream"], json!(true));
    assert_eq!(captured.body["prompt_cache_key"], json!("pi-session-123"));
    assert_eq!(captured.body["reasoning"], json!({ "effort": "medium" }));
    assert_eq!(captured.body["include"], json!(["reasoning.encrypted_content"]));
    assert!(captured.body.get("prompt_cache_retention").is_none());
    let has_dev = captured.body["input"].as_array().unwrap().iter().any(|item| {
        item.get("role").and_then(Value::as_str) == Some("developer")
            && item.get("content").and_then(Value::as_str) == Some("You are a careful coding assistant.")
    });
    assert!(has_dev);
}

#[tokio::test]
async fn requests_encrypted_reasoning_without_an_effort_override() {
    let grok = xai_models().get("grok-4.5").expect("grok-4.5");
    let captured = capture_request(
        grok,
        &Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert_eq!(captured.body["model"], json!("grok-4.5"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(captured.body["include"], json!(["reasoning.encrypted_content"]));
    assert!(captured.body.get("reasoning").is_none());
}

#[tokio::test]
async fn uses_responses_for_grok_4_7_with_xhigh_effort_and_encrypted_reasoning() {
    let grok = xai_models().get("grok-4.7").expect("grok-4.7");
    let captured = capture_request(
        grok,
        &Context {
            system_prompt: Some("You are a careful coding assistant.".to_owned()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }
        .with_extra("reasoningEffort", json!("xhigh")),
    )
    .await;

    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(captured.body["model"], json!("grok-4.7"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(captured.body["stream"], json!(true));
    assert_eq!(captured.body["reasoning"], json!({ "effort": "xhigh" }));
    assert_eq!(captured.body["include"], json!(["reasoning.encrypted_content"]));
}

#[tokio::test]
async fn uses_responses_for_grok_4_3() {
    let grok = xai_models().get("grok-4.3").expect("grok-4.3");
    let captured = capture_request(
        grok,
        &Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: 1,
            })],
            tools: None,
        },
        ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }
        .with_extra("reasoningEffort", json!("low")),
    )
    .await;

    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(captured.body["model"], json!("grok-4.3"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(captured.body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(captured.body["reasoning"], json!({ "effort": "low" }));
}

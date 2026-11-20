//! Port of packages/ai/test/github-copilot-anthropic.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::{Value, json};

use pi_ai::api::anthropic_messages::stream as stream_anthropic;
use pi_ai::api::github_copilot_headers::{
    build_copilot_dynamic_headers, has_copilot_vision_input, infer_copilot_initiator,
};
use pi_ai::models::get_supported_thinking_levels;
use pi_ai::providers::all::get_builtin_model;
use pi_ai::types::{
    ApiStreamOptions, AssistantMessage, AssistantMessageEvent, Context, FetchFunction, ImageContent, Message,
    ModelThinkingLevel, StopReason, TextContent, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Headers, Response};

fn sse_response() -> Response {
    let body = [
        format!(
            "event: message_start\ndata: {}\n",
            pi_js::json::stringify(&json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": { "input_tokens": 10, "output_tokens": 0 },
                },
            }))
        ),
        format!(
            "event: message_delta\ndata: {}\n",
            pi_js::json::stringify(&json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 5 },
            }))
        ),
    ]
    .join("\n");

    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

fn hello_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("You are a helpful assistant.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: 1,
        })],
        tools: None,
    })
}

#[test]
fn applies_copilot_specific_adaptive_thinking_effort_overrides() {
    let opus5 = get_builtin_model("github-copilot", "claude-opus-5").expect("claude-opus-5");
    assert_eq!(opus5.api, "anthropic-messages");
    assert_eq!(opus5.context_window, 1_000_000);
    let map5 = opus5.thinking_level_map.as_ref().expect("thinking_level_map");
    assert_eq!(map5.get(&ModelThinkingLevel::Minimal), Some(&Some("low".to_owned())));
    assert_eq!(map5.get(&ModelThinkingLevel::XHigh), Some(&Some("xhigh".to_owned())));
    assert_eq!(map5.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    let levels5 = get_supported_thinking_levels(&opus5);
    assert!(levels5.contains(&ModelThinkingLevel::XHigh));
    assert!(levels5.contains(&ModelThinkingLevel::Max));

    let opus55 = get_builtin_model("github-copilot", "claude-opus-5.5").expect("claude-opus-5.5");
    assert_eq!(opus55.api, "anthropic-messages");
    assert_eq!(opus55.context_window, 1_000_000);
    assert_eq!(
        get_supported_thinking_levels(&opus55),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
            ModelThinkingLevel::Max,
        ]
    );

    let sonnet46 = get_builtin_model("github-copilot", "claude-sonnet-4.6").expect("claude-sonnet-4.6");
    let map46 = sonnet46.thinking_level_map.as_ref().expect("thinking_level_map");
    assert_eq!(map46.get(&ModelThinkingLevel::Minimal), Some(&Some("low".to_owned())));
    assert_eq!(map46.get(&ModelThinkingLevel::Max), Some(&Some("max".to_owned())));
    let levels46 = get_supported_thinking_levels(&sonnet46);
    assert!(levels46.contains(&ModelThinkingLevel::Max));
    assert!(!levels46.contains(&ModelThinkingLevel::XHigh));
}

#[tokio::test]
async fn uses_bearer_auth_copilot_headers_and_valid_anthropic_messages_payload() {
    let model = get_builtin_model("github-copilot", "claude-sonnet-4.6").expect("claude-sonnet-4.6");
    assert_eq!(model.api, "anthropic-messages");

    let captured = Arc::new(Mutex::new(None::<(IndexMap<String, String>, Value)>));
    let captured_ref = Arc::clone(&captured);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let captured_ref = Arc::clone(&captured_ref);
        Box::pin(async move {
            let hdrs: IndexMap<String, String> = req.headers.entries().into_iter().collect();
            let body = req.body_json().unwrap_or(Value::Null);
            *captured_ref.lock().unwrap() = Some((hdrs, body));
            Ok(sse_response())
        })
    });

    let mut opts = ApiStreamOptions::default();
    opts.base.api_key = Some("tid_copilot_session_test_token".to_owned());
    opts.base.fetch = Some(fetch_fn);

    let mut stream = stream_anthropic(&model, &hello_context(), Some(opts));
    while let Some(event) = stream.next().await {
        if matches!(event, AssistantMessageEvent::Error { .. }) {
            break;
        }
    }

    let (headers, params) = captured.lock().unwrap().clone().expect("request must be sent");
    assert!(!headers.contains_key("x-api-key"));
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer tid_copilot_session_test_token")
    );
    assert!(
        headers
            .get("user-agent")
            .is_some_and(|ua| ua.contains("GitHubCopilotChat"))
    );
    assert_eq!(
        headers.get("copilot-integration-id").map(String::as_str),
        Some("vscode-chat")
    );
    assert_eq!(headers.get("x-initiator").map(String::as_str), Some("user"));
    assert_eq!(
        headers.get("openai-intent").map(String::as_str),
        Some("conversation-edits")
    );

    let beta_header = headers.get("anthropic-beta").map(String::as_str).unwrap_or("");
    assert!(!beta_header.contains("fine-grained-tool-streaming-2025-05-14"));

    assert_eq!(params["model"], "claude-sonnet-4.6");
    assert_eq!(params["stream"], true);
    assert_eq!(params["max_tokens"], model.max_tokens);
    assert!(params["messages"].is_array());
}

#[tokio::test]
async fn omits_interleaved_thinking_beta_for_adaptive_thinking_models() {
    let model = get_builtin_model("github-copilot", "claude-sonnet-4.6").expect("claude-sonnet-4.6");
    let captured_headers = Arc::new(Mutex::new(None::<IndexMap<String, String>>));
    let headers_ref = Arc::clone(&captured_headers);

    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let headers_ref = Arc::clone(&headers_ref);
        Box::pin(async move {
            *headers_ref.lock().unwrap() = Some(req.headers.entries().into_iter().collect());
            Ok(sse_response())
        })
    });

    let mut opts = ApiStreamOptions::default().with_extra("interleavedThinking", json!(true));
    opts.base.api_key = Some("tid_copilot_session_test_token".to_owned());
    opts.base.fetch = Some(fetch_fn);

    let mut stream = stream_anthropic(&model, &hello_context(), Some(opts));
    while let Some(event) = stream.next().await {
        if matches!(event, AssistantMessageEvent::Error { .. }) {
            break;
        }
    }

    let headers = captured_headers.lock().unwrap().clone().expect("request headers");
    let beta = headers.get("anthropic-beta").map(String::as_str).unwrap_or("");
    assert!(!beta.contains("interleaved-thinking-2025-05-14"));
}

#[test]
fn dynamic_copilot_header_helpers_infer_initiator_and_vision() {
    let user_msg = Message::User(UserMessage {
        content: UserMessageContent::Text("hi".to_owned()),
        timestamp: 1,
    });
    let assistant_msg = Message::Assistant(AssistantMessage {
        content: Vec::new(),
        api: "anthropic-messages".to_owned(),
        provider: "github-copilot".to_owned(),
        model: "claude-sonnet-4.6".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    });

    assert_eq!(infer_copilot_initiator(&[]), "user");
    assert_eq!(infer_copilot_initiator(std::slice::from_ref(&user_msg)), "user");
    assert_eq!(
        infer_copilot_initiator(&[user_msg.clone(), assistant_msg.clone()]),
        "agent"
    );
    assert!(!has_copilot_vision_input(std::slice::from_ref(&user_msg)));

    let image_user = Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![
            UserContentBlock::Text(TextContent::new("look")),
            UserContentBlock::Image(ImageContent::new("abcd", "image/png")),
        ]),
        timestamp: 3,
    });
    assert!(has_copilot_vision_input(std::slice::from_ref(&image_user)));
    let headers = build_copilot_dynamic_headers(&[image_user], true);
    assert_eq!(headers.get("X-Initiator").map(String::as_str), Some("user"));
    assert_eq!(
        headers.get("Openai-Intent").map(String::as_str),
        Some("conversation-edits")
    );
    assert_eq!(headers.get("Copilot-Vision-Request").map(String::as_str), Some("true"));
}

//! Port of packages/ai/test/openai-completions-reasoning-details.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCost, ModelInput,
    StreamOptions, ThinkingContent, Tool, ToolCall,
};
use pi_js::fetch::{Headers, Response};
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn reasoning_detail() -> Value {
    json!({ "type": "reasoning.encrypted", "id": "call_1", "data": "encrypted-signature" })
}

fn signed_reasoning_text_detail() -> Value {
    json!({
        "type": "reasoning.text",
        "text": "I should call the read tool.",
        "signature": "sha256:signed-text",
        "id": "reasoning-text-1",
        "format": "anthropic-claude-v1",
        "index": 0,
    })
}

fn reasoning_summary_detail() -> Value {
    json!({
        "type": "reasoning.summary",
        "summary": "Decided to inspect the requested file.",
        "id": "reasoning-summary-1",
        "format": "anthropic-claude-v1",
        "index": 1,
    })
}

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "google/gemini-test".to_owned(),
        name: "Gemini Test".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 100_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-test",
        "model": "google/gemini-test",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    })
}

fn tool_call_chunk() -> Value {
    chunk(
        json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" },
            }]
        }),
        None,
    )
}

fn sse_from_chunks(chunks: &[Value]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str(&format!("data: {c}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

async fn run_stream(messages: Vec<AssistantMessage>, chunks: Vec<Value>) -> (AssistantMessage, Value) {
    let payload = Arc::new(Mutex::new(None::<Value>));
    let p_c = payload.clone();
    let sse = sse_from_chunks(&chunks);

    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: messages.into_iter().map(Message::Assistant).collect(),
        tools: Some(vec![Tool {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: t::object([("path", t::string())]),
            constrained_sampling: None,
        }]),
    });

    let msg = stream_openai_completions(
        &test_model(),
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_owned()),
                on_payload: Some(Arc::new(move |params, _| {
                    let p = p_c.clone();
                    Box::pin(async move {
                        *p.lock().unwrap() = Some(params);
                        Ok(None)
                    })
                })),
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

    (msg, payload.lock().unwrap().take().unwrap())
}

fn get_assistant_payload(payload: &Value) -> &Value {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|m| m.get("role") == Some(&json!("assistant")))
        .unwrap()
}

#[tokio::test]
async fn preserves_reasoning_details_in_the_thinking_signature() {
    let detail = reasoning_detail();
    let (assistant, _) = run_stream(
        Vec::new(),
        vec![
            chunk(json!({ "reasoning_details": [detail.clone()] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ],
    )
    .await;

    let thinking = assistant
        .content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        thinking,
        &ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some(pi_js::json::stringify(&Value::Array(vec![detail.clone()]))),
            redacted: None,
        }
    );

    let tool_call = assistant
        .content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .unwrap();
    let mut expected_args = IndexMap::new();
    expected_args.insert("path".to_owned(), json!("README.md"));
    assert_eq!(tool_call, &ToolCall::new("call_1", "read", expected_args));

    let (_, payload2) = run_stream(
        vec![assistant],
        vec![chunk(json!({ "content": "ok" }), None), chunk(json!({}), Some("stop"))],
    )
    .await;
    assert_eq!(
        get_assistant_payload(&payload2).get("reasoning_details"),
        Some(&Value::Array(vec![detail]))
    );
}

#[tokio::test]
async fn falls_back_to_encrypted_tool_call_signatures_for_older_stored_assistant_messages() {
    let detail = reasoning_detail();
    let (mut assistant, _) = run_stream(
        Vec::new(),
        vec![
            chunk(json!({ "reasoning_details": [detail.clone()] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ],
    )
    .await;

    assistant
        .content
        .retain(|b| !matches!(b, AssistantContentBlock::Thinking(_)));
    for block in &mut assistant.content {
        if let AssistantContentBlock::ToolCall(tc) = block {
            tc.thought_signature = Some(pi_js::json::stringify(&detail));
        }
    }

    let (_, payload2) = run_stream(
        vec![assistant],
        vec![chunk(json!({ "content": "ok" }), None), chunk(json!({}), Some("stop"))],
    )
    .await;

    assert_eq!(
        get_assistant_payload(&payload2).get("reasoning_details"),
        Some(&Value::Array(vec![detail]))
    );
}

#[tokio::test]
async fn preserves_signed_text_and_summary_reasoning_details_in_original_sequence() {
    let detail = reasoning_detail();
    let signed = signed_reasoning_text_detail();
    let summary = reasoning_summary_detail();

    let (assistant, _) = run_stream(
        Vec::new(),
        vec![
            chunk(
                json!({
                    "reasoning": signed["text"],
                    "reasoning_details": [signed.clone()],
                }),
                None,
            ),
            chunk(json!({ "reasoning_details": [detail.clone(), summary.clone()] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ],
    )
    .await;

    let expected = vec![signed.clone(), detail, summary];
    let thinking = assistant
        .content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(thinking.thinking, signed["text"].as_str().unwrap());
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(pi_js::json::stringify(&Value::Array(expected.clone())).as_str())
    );

    let (_, payload2) = run_stream(
        vec![assistant],
        vec![chunk(json!({ "content": "ok" }), None), chunk(json!({}), Some("stop"))],
    )
    .await;

    let assistant_payload = get_assistant_payload(&payload2);
    assert_eq!(
        assistant_payload.get("reasoning_details"),
        Some(&Value::Array(expected))
    );
    assert!(assistant_payload.get("reasoning").is_none());
}

#[tokio::test]
async fn merges_consecutive_text_and_summary_reasoning_details_deltas_before_replay() {
    let text_delta = json!({ "type": "reasoning.text", "text": "The", "index": 0 });
    let text_delta_sig = json!({
        "type": "reasoning.text",
        "text": " user wants the time.",
        "signature": "sha256:text-signature",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let summary_delta = json!({ "type": "reasoning.summary", "summary": "Looked", "index": 0 });
    let summary_delta_fmt = json!({
        "type": "reasoning.summary",
        "summary": " up time.",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let later_summary_delta = json!({
        "type": "reasoning.summary",
        "summary": "After encrypted block.",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let detail = reasoning_detail();

    let expected = vec![
        json!({
            "type": "reasoning.text",
            "text": "The user wants the time.",
            "index": 0,
            "signature": "sha256:text-signature",
            "format": "openai-responses-v1",
        }),
        json!({
            "type": "reasoning.summary",
            "summary": "Looked up time.",
            "index": 0,
            "format": "openai-responses-v1",
        }),
        detail.clone(),
        later_summary_delta.clone(),
    ];

    let (assistant, _) = run_stream(
        Vec::new(),
        vec![
            chunk(json!({ "reasoning_details": [text_delta] }), None),
            chunk(json!({ "reasoning_details": [text_delta_sig] }), None),
            chunk(json!({ "reasoning_details": [summary_delta] }), None),
            chunk(json!({ "reasoning_details": [summary_delta_fmt] }), None),
            chunk(json!({ "reasoning_details": [detail] }), None),
            chunk(json!({ "reasoning_details": [later_summary_delta] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ],
    )
    .await;

    let thinking = assistant
        .content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(thinking.thinking, "");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(pi_js::json::stringify(&Value::Array(expected.clone())).as_str())
    );

    let (_, payload2) = run_stream(
        vec![assistant],
        vec![chunk(json!({ "content": "ok" }), None), chunk(json!({}), Some("stop"))],
    )
    .await;

    assert_eq!(
        get_assistant_payload(&payload2).get("reasoning_details"),
        Some(&Value::Array(expected))
    );
}

// https://github.com/earendil-works/pi/issues/10926
#[tokio::test]
async fn places_thinking_before_text_when_one_delta_carries_reasoning_content_or_details_and_content() {
    let (msg1, _) = run_stream(
        Vec::new(),
        vec![chunk(
            json!({ "role": "assistant", "reasoning_content": "thinking", "content": "answer" }),
            Some("stop"),
        )],
    )
    .await;
    assert_eq!(
        msg1.content,
        vec![
            AssistantContentBlock::Thinking(pi_ai::types::ThinkingContent {
                thinking: "thinking".to_owned(),
                thinking_signature: Some("reasoning_content".to_owned()),
                redacted: None,
            }),
            AssistantContentBlock::Text(pi_ai::types::TextContent::new("answer")),
        ]
    );

    let (msg2, _) = run_stream(
        Vec::new(),
        vec![chunk(
            json!({ "reasoning_details": [reasoning_detail()], "content": "answer" }),
            Some("stop"),
        )],
    )
    .await;
    let types: Vec<&str> = msg2
        .content
        .iter()
        .map(|b| match b {
            AssistantContentBlock::Thinking(_) => "thinking",
            AssistantContentBlock::Text(_) => "text",
            AssistantContentBlock::ToolCall(_) => "toolCall",
        })
        .collect();
    assert_eq!(types, vec!["thinking", "text"]);
}

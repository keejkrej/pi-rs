//! Port of packages/ai/test/bedrock-redacted-reasoning.test.ts

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStopEvent, ConversationRole, ConverseStreamOutput,
    MessageStartEvent, MessageStopEvent, ReasoningContentBlockDelta, StopReason as BedrockStopReason,
};
use aws_smithy_types::Blob;
use indexmap::IndexMap;
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportEvent, BedrockTransportOutcome, BedrockTransportStreamResponse,
    override_bedrock_transport, stream_with_options,
};
use pi_ai::types::{
    Api, AssistantContentBlock, AssistantMessage, CacheRetention, Context, JsonObject, JsonValue, KnownApi,
    KnownProvider, Message, Model, ModelCost, ModelInput, ProviderId, StopReason, StreamOptions, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use serde_json::json;

const REDACTED_BASE64: &str = "cnNuXzVaVnJpZjRKMGJYSXFtV2RsZWRqN1FJRmVOaWtSUWJF";

fn redacted_bytes() -> Vec<u8> {
    pi_js::b64::atob_bytes(REDACTED_BASE64).expect("valid base64 fixture")
}

fn gpt_model() -> Model {
    Model {
        r#type: None,
        id: "global.openai.gpt-5.6-terra".to_string(),
        name: "GPT-5.6 Terra (Global)".to_string(),
        api: Api::from(KnownApi::BedrockConverseStream),
        provider: ProviderId::from(KnownProvider::AmazonBedrock),
        base_url: "https://bedrock-runtime.ap-northeast-1.amazonaws.com".to_string(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 400_000,
        max_tokens: 128_000,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn empty_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

fn redacted_reasoning_events() -> Vec<BedrockTransportEvent> {
    let bytes = redacted_bytes();
    vec![
        BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
            MessageStartEvent::builder()
                .role(ConversationRole::Assistant)
                .build()
                .expect("message start"),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(0)
                .delta(ContentBlockDelta::ReasoningContent(
                    ReasoningContentBlockDelta::RedactedContent(Blob::new(bytes)),
                ))
                .build()
                .expect("reasoning delta"),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
            ContentBlockStopEvent::builder()
                .content_block_index(0)
                .build()
                .expect("block stop 0"),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(1)
                .delta(ContentBlockDelta::Text("done".to_string()))
                .build()
                .expect("text delta"),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
            ContentBlockStopEvent::builder()
                .content_block_index(1)
                .build()
                .expect("block stop 1"),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
            MessageStopEvent::builder()
                .stop_reason(BedrockStopReason::EndTurn)
                .build()
                .expect("message stop"),
        )),
    ]
}

fn hello_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    })
}

async fn capture_payload(context: Context) -> JsonValue {
    let captured: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);

    let options = BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            signal: Some(pi_js::AbortSignal::abort(None)),
            on_payload: Some(Arc::new(move |payload, _model| {
                let captured_inner = Arc::clone(&captured_ref);
                Box::pin(async move {
                    *captured_inner.lock().unwrap() = Some(payload.clone());
                    Ok(Some(payload))
                })
            })),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    };

    let normalized = normalize_context(&context);
    let _ = stream_with_options(&gpt_model(), &normalized, Some(options))
        .result()
        .await;

    captured
        .lock()
        .unwrap()
        .clone()
        .expect("expected Bedrock payload to be captured before request abort")
}

#[tokio::test]
async fn does_not_fail_stream_when_reasoning_arrives_as_redacted_content() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: IndexMap::new(),
                events: redacted_reasoning_events(),
            }))
        })
    });

    let response = stream_with_options(&gpt_model(), &hello_context(), None).result().await;

    assert_ne!(response.stop_reason, StopReason::Error, "{:?}", response.error_message);
    let types: Vec<&str> = response
        .content
        .iter()
        .map(|c| match c {
            AssistantContentBlock::Thinking(_) => "thinking",
            AssistantContentBlock::Text(_) => "text",
            AssistantContentBlock::ToolCall(_) => "toolCall",
        })
        .collect();
    assert_eq!(types, vec!["thinking", "text"]);
    assert_eq!(
        response.content[1],
        AssistantContentBlock::Text(TextContent::new("done"))
    );
}

#[tokio::test]
async fn preserves_encrypted_reasoning_payload_on_assistant_message() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: IndexMap::new(),
                events: redacted_reasoning_events(),
            }))
        })
    });

    let response = stream_with_options(&gpt_model(), &hello_context(), None).result().await;

    let thinking = response.content.iter().find_map(|c| match c {
        AssistantContentBlock::Thinking(th) => Some(th),
        _ => None,
    });
    let th = thinking.expect("thinking block");
    assert_eq!(th.redacted, Some(true));
    assert_eq!(th.thinking_signature.as_deref(), Some(REDACTED_BASE64));

    let serialized: JsonValue = pi_js::json::parse(&pi_js::json::stringify(th)).unwrap();
    assert!(serialized.get("redactedChunks").is_none());
}

#[tokio::test]
async fn encodes_payload_when_stream_never_sends_content_block_stop() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let bytes = redacted_bytes();
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
                    ContentBlockDeltaEvent::builder()
                        .content_block_index(0)
                        .delta(ContentBlockDelta::ReasoningContent(
                            ReasoningContentBlockDelta::RedactedContent(Blob::new(bytes)),
                        ))
                        .build()
                        .expect("reasoning delta"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::EndTurn)
                        .build()
                        .expect("message stop"),
                )),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let response = stream_with_options(&gpt_model(), &hello_context(), None).result().await;

    let th = response
        .content
        .iter()
        .find_map(|c| match c {
            AssistantContentBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .expect("thinking block");
    assert_eq!(th.thinking_signature.as_deref(), Some(REDACTED_BASE64));
    let serialized: JsonValue = pi_js::json::parse(&pi_js::json::stringify(th)).unwrap();
    assert!(serialized.get("redactedChunks").is_none());
    assert!(serialized.get("index").is_none());
}

#[tokio::test]
async fn joins_encrypted_reasoning_split_across_deltas() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let full = redacted_bytes();
            let head = full[..7].to_vec();
            let tail = full[7..].to_vec();
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
                    ContentBlockDeltaEvent::builder()
                        .content_block_index(0)
                        .delta(ContentBlockDelta::ReasoningContent(
                            ReasoningContentBlockDelta::RedactedContent(Blob::new(head)),
                        ))
                        .build()
                        .expect("head delta"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
                    ContentBlockDeltaEvent::builder()
                        .content_block_index(0)
                        .delta(ContentBlockDelta::ReasoningContent(
                            ReasoningContentBlockDelta::RedactedContent(Blob::new(tail)),
                        ))
                        .build()
                        .expect("tail delta"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
                    ContentBlockStopEvent::builder()
                        .content_block_index(0)
                        .build()
                        .expect("block stop"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::EndTurn)
                        .build()
                        .expect("message stop"),
                )),
            ];
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: IndexMap::new(),
                events,
            }))
        })
    });

    let response = stream_with_options(&gpt_model(), &hello_context(), None).result().await;

    let th = response
        .content
        .iter()
        .find_map(|c| match c {
            AssistantContentBlock::Thinking(t) => Some(t),
            _ => None,
        })
        .expect("thinking block");
    assert_eq!(th.thinking_signature.as_deref(), Some(REDACTED_BASE64));
    assert_eq!(th.thinking, "[Reasoning redacted]");
}

#[tokio::test]
async fn replays_redacted_reasoning_as_reasoning_content_redacted_content() {
    let model = gpt_model();
    let payload = capture_payload(Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_string()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: Some(REDACTED_BASE64.to_string()),
                        redacted: Some(true),
                    }),
                    AssistantContentBlock::Text(TextContent::new("done")),
                ],
                api: Api::from(KnownApi::BedrockConverseStream),
                provider: ProviderId::from(KnownProvider::AmazonBedrock),
                model: model.id,
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: empty_usage(),
                stop_reason: StopReason::Stop,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 2,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("continue".to_string()),
                timestamp: 3,
            }),
        ],
        tools: None,
    })
    .await;

    let assistant = payload["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message");

    assert_eq!(
        assistant["content"],
        json!([
            { "reasoningContent": { "redactedContent": redacted_bytes() } },
            { "text": "done" }
        ])
    );
}

#[tokio::test]
async fn replays_redacted_reasoning_before_tool_use_block_it_belongs_to() {
    let model = gpt_model();
    let mut read_args = JsonObject::new();
    read_args.insert("path".to_string(), json!("/tmp/a.txt"));

    let payload = capture_payload(Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("read the file".to_string()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: Some(REDACTED_BASE64.to_string()),
                        redacted: Some(true),
                    }),
                    AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "read", read_args)),
                ],
                api: Api::from(KnownApi::BedrockConverseStream),
                provider: ProviderId::from(KnownProvider::AmazonBedrock),
                model: model.id,
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: empty_usage(),
                stop_reason: StopReason::ToolUse,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 2,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "tool-1".to_string(),
                tool_name: "read".to_string(),
                content: vec![UserContentBlock::Text(TextContent::new("file body"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 3,
            }),
        ],
        tools: None,
    })
    .await;

    let assistant = payload["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message");

    assert_eq!(
        assistant["content"],
        json!([
            { "reasoningContent": { "redactedContent": redacted_bytes() } },
            { "toolUse": { "toolUseId": "tool-1", "name": "read", "input": { "path": "/tmp/a.txt" } } }
        ])
    );
}

//! Port of packages/ai/test/bedrock-convert-messages.test.ts

use std::sync::{Arc, Mutex};

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart, ContentBlockStartEvent, ContentBlockStopEvent,
    ConversationRole, ConverseStreamOutput, MessageStartEvent, MessageStopEvent, StopReason as BedrockStopReason,
    ToolUseBlockDelta, ToolUseBlockStart,
};
use indexmap::IndexMap;
use pi_ai::Type;
use pi_ai::api::bedrock_converse_stream::{
    BedrockOptions, BedrockTransportEvent, BedrockTransportOutcome, BedrockTransportStreamResponse,
    override_bedrock_transport, stream_with_options,
};
use pi_ai::types::{
    Api, AssistantContentBlock, AssistantMessage, BedrockCompat, CacheRetention, ConstrainedSampling,
    ConstrainedSamplingConfig, Context, ImageContent, JsonObject, JsonSchemaStrict, JsonValue, KnownApi, KnownProvider,
    Message, Model, ModelCompat, ModelCost, ModelInput, ProviderId, StopReason, StreamOptions, TextContent, Tool,
    ToolCall, ToolResultMessage, Usage, UsageCost, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use serde_json::json;

fn base_model() -> Model {
    Model {
        r#type: None,
        id: "us.anthropic.claude-sonnet-4-5-20250929-v1:0".to_string(),
        name: "Claude Sonnet 4.5 (US)".to_string(),
        api: Api::from(KnownApi::BedrockConverseStream),
        provider: ProviderId::from(KnownProvider::AmazonBedrock),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".to_string(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens: 64_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::Bedrock(BedrockCompat {
            supports_strict_mode: Some(true),
        })),
        sampling_params_by_thinking_level: None,
    }
}

fn nova_model() -> Model {
    Model {
        id: "amazon.nova-lite-v1:0".to_string(),
        name: "Nova Lite".to_string(),
        reasoning: false,
        compat: None,
        ..base_model()
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

async fn capture_payload(context: Context, model: &Model) -> JsonValue {
    let captured: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);

    let options = BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            signal: Some(pi_js::abort::AbortSignal::abort(None)),
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
    let _ = stream_with_options(model, &normalized, Some(options)).result().await;
    captured.lock().unwrap().clone().expect("expected payload capture")
}

#[tokio::test]
async fn gates_native_strict_tool_use_by_model_capability() {
    let base = base_model();
    let nova = nova_model();

    let mut tool = Tool {
        name: "lookup".to_string(),
        description: "Look up a value".to_string(),
        parameters: Type::object([("value", Type::string())]),
        constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrict::Require,
        })),
    };

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the tool".to_string()),
            timestamp: 1,
        })],
        tools: Some(vec![tool.clone()]),
    };

    let payload = capture_payload(context, &base).await;
    assert_eq!(payload["toolConfig"]["tools"][0]["toolSpec"]["strict"], json!(true));

    tool.constrained_sampling = Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
        strict: JsonSchemaStrict::Prefer,
    }));
    let nova_context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the tool".to_string()),
            timestamp: 1,
        })],
        tools: Some(vec![tool]),
    };
    let nova_payload = capture_payload(nova_context, &nova).await;
    assert!(
        nova_payload["toolConfig"]["tools"][0]["toolSpec"]
            .get("strict")
            .is_none()
    );
}

#[tokio::test]
async fn preserves_empty_property_names_in_streamed_tool_arguments() {
    let _guard = override_bedrock_transport(|_req| {
        Box::pin(async {
            let events = vec![
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
                    MessageStartEvent::builder()
                        .role(ConversationRole::Assistant)
                        .build()
                        .expect("message start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStart(
                    ContentBlockStartEvent::builder()
                        .content_block_index(0)
                        .start(ContentBlockStart::ToolUse(
                            ToolUseBlockStart::builder()
                                .tool_use_id("tool-1")
                                .name("edit")
                                .build()
                                .expect("tool use start"),
                        ))
                        .build()
                        .expect("block start"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
                    ContentBlockDeltaEvent::builder()
                        .content_block_index(0)
                        .delta(ContentBlockDelta::ToolUse(
                            ToolUseBlockDelta::builder()
                                .input(r#"{"path":"/workspace/foobar/file.js","edits":[{"oldText":"first","newText":"updated first"},{"oldText":"second","newText":"updated second","":""}]}"#)
                                .build()
                                .expect("tool delta"),
                        ))
                        .build()
                        .expect("block delta"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
                    ContentBlockStopEvent::builder()
                        .content_block_index(0)
                        .build()
                        .expect("block stop"),
                )),
                BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
                    MessageStopEvent::builder()
                        .stop_reason(BedrockStopReason::ToolUse)
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

    let model = base_model();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the tool".to_string()),
            timestamp: 1,
        })],
        tools: None,
    });
    let options = BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    };

    let message = stream_with_options(&model, &context, Some(options)).result().await;

    let mut expected_args = JsonObject::new();
    expected_args.insert("path".to_string(), json!("/workspace/foobar/file.js"));
    expected_args.insert(
        "edits".to_string(),
        json!([
            { "oldText": "first", "newText": "updated first" },
            { "oldText": "second", "newText": "updated second", "": "" }
        ]),
    );
    assert_eq!(
        message.content[0],
        AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "edit", expected_args))
    );
}

#[tokio::test]
async fn replaces_blank_user_string_content_with_placeholder() {
    let model = base_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("   ".to_string()),
                timestamp: 1,
            })],
            tools: None,
        },
        &model,
    )
    .await;

    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(payload["messages"][0]["content"], json!([{ "text": "<empty>" }]));
}

#[tokio::test]
async fn filters_blank_user_text_blocks_when_other_content_remains() {
    let model = base_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Blocks(vec![
                    UserContentBlock::Text(TextContent::new("")),
                    UserContentBlock::Text(TextContent::new("hello")),
                ]),
                timestamp: 1,
            })],
            tools: None,
        },
        &model,
    )
    .await;

    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(payload["messages"][0]["content"], json!([{ "text": "hello" }]));
}

#[tokio::test]
async fn replaces_blank_tool_result_content_with_placeholder() {
    let model = base_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(ToolResultMessage {
                tool_call_id: "tool-1".to_string(),
                tool_name: "tool".to_string(),
                content: vec![UserContentBlock::Text(TextContent::new(""))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            })],
            tools: None,
        },
        &model,
    )
    .await;

    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["messages"][0]["content"][0]["toolResult"]["content"],
        json!([{ "text": "<empty>" }])
    );
}

#[tokio::test]
async fn skips_assistant_messages_with_only_blank_text_blocks() {
    // PORT: In Rust `UserContentBlock` and `AssistantContentBlock` are typed enums (no
    // untyped `{ type: "unknown" }` variant); blank text blocks verify the same empty-block
    // assistant-message pruning guard.
    let model = base_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::Text(TextContent::new("   "))],
                api: Api::from(KnownApi::BedrockConverseStream),
                provider: ProviderId::from(KnownProvider::AmazonBedrock),
                model: model.id.clone(),
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
                timestamp: 1,
                duration_ms: None,
            })],
            tools: None,
        },
        &model,
    )
    .await;

    assert_eq!(payload["messages"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn removes_empty_property_names_only_from_replayed_bedrock_input() {
    let model = base_model();
    let mut tool_arguments = JsonObject::new();
    tool_arguments.insert("path".to_string(), json!("/workspace/foobar/file.js"));
    tool_arguments.insert(
        "edits".to_string(),
        json!([
            { "oldText": "first", "newText": "updated first" },
            { "oldText": "second", "newText": "updated second", "": "" }
        ]),
    );

    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                        "tool-1",
                        "edit",
                        tool_arguments.clone(),
                    ))],
                    api: Api::from(KnownApi::BedrockConverseStream),
                    provider: ProviderId::from(KnownProvider::AmazonBedrock),
                    model: model.id.clone(),
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
                    timestamp: 1,
                    duration_ms: None,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "tool-1".to_string(),
                    tool_name: "edit".to_string(),
                    content: vec![UserContentBlock::Text(TextContent::new("done"))],
                    details: None,
                    usage: None,
                    nested_calls: None,
                    is_error: false,
                    timestamp: 2,
                    duration_ms: None,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("Continue".to_string()),
                    timestamp: 3,
                }),
            ],
            tools: None,
        },
        &model,
    )
    .await;

    assert_eq!(
        payload["messages"][0]["content"][0]["toolUse"]["input"],
        json!({
            "path": "/workspace/foobar/file.js",
            "edits": [
                { "oldText": "first", "newText": "updated first" },
                { "oldText": "second", "newText": "updated second" }
            ]
        })
    );
    assert_eq!(
        tool_arguments["edits"][1],
        json!({ "oldText": "second", "newText": "updated second", "": "" })
    );
}

// Regression for #10794: Claude rejects error tool results that contain images.
#[tokio::test]
async fn moves_images_of_error_tool_results_after_the_tool_results() {
    let model = base_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![
                Message::Assistant(AssistantMessage {
                    content: vec![
                        AssistantContentBlock::ToolCall(ToolCall::new("call-1", "codemode", JsonObject::new())),
                        AssistantContentBlock::ToolCall(ToolCall::new("call-2", "read", JsonObject::new())),
                    ],
                    api: Api::from(KnownApi::BedrockConverseStream),
                    provider: ProviderId::from(KnownProvider::AmazonBedrock),
                    model: model.id.clone(),
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
                    timestamp: 1,
                    duration_ms: None,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "call-1".to_string(),
                    tool_name: "codemode".to_string(),
                    content: vec![
                        UserContentBlock::Text(TextContent::new("Script failed")),
                        UserContentBlock::Image(ImageContent::new("AAAA", "image/png")),
                        UserContentBlock::Text(TextContent::new("Script error")),
                    ],
                    details: None,
                    usage: None,
                    nested_calls: None,
                    is_error: true,
                    timestamp: 2,
                    duration_ms: None,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "call-2".to_string(),
                    tool_name: "read".to_string(),
                    content: vec![UserContentBlock::Image(ImageContent::new("BBBB", "image/png"))],
                    details: None,
                    usage: None,
                    nested_calls: None,
                    is_error: false,
                    timestamp: 3,
                    duration_ms: None,
                }),
            ],
            tools: None,
        },
        &model,
    )
    .await;

    let content = payload["messages"][1]["content"].as_array().unwrap();
    assert_eq!(content.len(), 4);
    assert_eq!(
        content[0]["toolResult"],
        json!({
            "toolUseId": "call-1",
            "content": [
                { "text": "Script failed" },
                { "text": "(image attached after the tool results)" },
                { "text": "Script error" },
            ],
            "status": "error",
        })
    );
    assert_eq!(content[1]["toolResult"]["status"], json!("success"));
    assert_eq!(content[1]["toolResult"]["content"][0]["image"]["format"], json!("png"));
    assert_eq!(
        content[2],
        json!({ "text": "Images from the error result of tool call call-1 (codemode):" })
    );
    assert_eq!(content[3]["image"]["format"], json!("png"));
}

#[tokio::test]
async fn keeps_images_in_error_tool_results_for_non_claude_models() {
    let model = nova_model();
    let payload = capture_payload(
        Context {
            system_prompt: None,
            messages: vec![Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".to_string(),
                tool_name: "codemode".to_string(),
                content: vec![
                    UserContentBlock::Text(TextContent::new("Script failed")),
                    UserContentBlock::Image(ImageContent::new("AAAA", "image/png")),
                ],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: true,
                timestamp: 1,
                duration_ms: None,
            })],
            tools: None,
        },
        &model,
    )
    .await;

    let content = payload["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["toolResult"]["toolUseId"], json!("call-1"));
    assert_eq!(content[0]["toolResult"]["status"], json!("error"));
    assert_eq!(
        content[0]["toolResult"]["content"][0],
        json!({ "text": "Script failed" })
    );
    assert_eq!(content[0]["toolResult"]["content"][1]["image"]["format"], json!("png"));
}

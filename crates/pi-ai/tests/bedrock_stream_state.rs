//! Port of packages/ai/test/bedrock-stream-state.test.ts

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart, ContentBlockStartEvent, ContentBlockStopEvent,
    ConversationRole, ConverseStreamOutput, MessageStartEvent, MessageStopEvent, ReasoningContentBlockDelta,
    StopReason as BedrockStopReason, ToolUseBlockDelta, ToolUseBlockStart,
};
use aws_smithy_types::Blob;
use futures::StreamExt;
use pi_ai::api::bedrock_converse_stream::{
    BedrockTransportEvent, BedrockTransportOutcome, BedrockTransportStreamResponse, override_bedrock_transport, stream,
};
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, KnownApi, KnownProvider, Message,
    Model, ModelCost, ModelInput, ProviderId, StopReason, ToolCall, UserMessage, UserMessageContent,
};
use serde_json::{Value, json};
use serial_test::serial;

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "global.anthropic.claude-test".to_owned(),
        name: "Claude Test".to_owned(),
        api: Api::from(KnownApi::BedrockConverseStream),
        provider: ProviderId::from(KnownProvider::AmazonBedrock),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 64_000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn write_call(size: usize) -> (Value, String) {
    let content: String = (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
        .collect();
    let sliced: String = content.chars().take(size).collect();
    let args = json!({ "path": "/src/file.ts", "content": sliced });
    let text = pi_js::json::stringify(&args);
    (args, text)
}

fn events(text: &str) -> Vec<BedrockTransportEvent> {
    let mut out = vec![
        BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStart(
            MessageStartEvent::builder()
                .role(ConversationRole::Assistant)
                .build()
                .unwrap(),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(0)
                .delta(ContentBlockDelta::ReasoningContent(
                    ReasoningContentBlockDelta::RedactedContent(Blob::new(vec![1_u8, 2])),
                ))
                .build()
                .unwrap(),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(0)
                .delta(ContentBlockDelta::ReasoningContent(
                    ReasoningContentBlockDelta::RedactedContent(Blob::new(vec![3_u8])),
                ))
                .build()
                .unwrap(),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
            ContentBlockStopEvent::builder().content_block_index(0).build().unwrap(),
        )),
        BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStart(
            ContentBlockStartEvent::builder()
                .content_block_index(1)
                .start(ContentBlockStart::ToolUse(
                    ToolUseBlockStart::builder()
                        .tool_use_id("tool_1")
                        .name("write")
                        .build()
                        .unwrap(),
                ))
                .build()
                .unwrap(),
        )),
    ];
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(32) {
        let delta: String = chunk.iter().collect();
        out.push(BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(1)
                .delta(ContentBlockDelta::ToolUse(
                    ToolUseBlockDelta::builder().input(delta).build().unwrap(),
                ))
                .build()
                .unwrap(),
        )));
    }
    out.push(BedrockTransportEvent::Sdk(ConverseStreamOutput::ContentBlockStop(
        ContentBlockStopEvent::builder().content_block_index(1).build().unwrap(),
    )));
    out.push(BedrockTransportEvent::Sdk(ConverseStreamOutput::MessageStop(
        MessageStopEvent::builder()
            .stop_reason(BedrockStopReason::ToolUse)
            .build()
            .unwrap(),
    )));
    out
}

fn expect_no_stream_state(message: &AssistantMessage) {
    for block in &message.content {
        let val = serde_json::to_value(block).unwrap();
        let obj = val.as_object().unwrap();
        let allowed: &[&str] = match obj.get("type").and_then(Value::as_str).unwrap() {
            "text" => &["type", "text", "textSignature"],
            "thinking" => &["type", "thinking", "thinkingSignature", "redacted"],
            "toolCall" => &["type", "id", "name", "arguments", "argumentsError"],
            other => panic!("unexpected block type {other}"),
        };
        for key in obj.keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected key {key} on block");
        }
    }
}

#[tokio::test]
#[serial]
async fn keeps_stream_state_off_partial_blocks_and_finishes_with_exact_arguments() {
    let model = test_model();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("write".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    });
    let (args, text) = write_call(5_000);
    let expected_content = args["content"].as_str().unwrap();

    let wire = events(&text);
    let _guard = override_bedrock_transport(move |_| {
        let evs = wire.clone();
        Box::pin(async move {
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: indexmap::IndexMap::new(),
                events: evs,
            }))
        })
    });

    let mut s = stream(&model, &context, None);
    let mut shown: isize = -1;
    while let Some(ev) = s.next().await {
        match &ev {
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => continue,
            other => {
                let partial = match other {
                    AssistantMessageEvent::Start { partial }
                    | AssistantMessageEvent::TextStart { partial, .. }
                    | AssistantMessageEvent::TextDelta { partial, .. }
                    | AssistantMessageEvent::TextEnd { partial, .. }
                    | AssistantMessageEvent::ThinkingStart { partial, .. }
                    | AssistantMessageEvent::ThinkingDelta { partial, .. }
                    | AssistantMessageEvent::ThinkingEnd { partial, .. }
                    | AssistantMessageEvent::ToolCallStart { partial, .. }
                    | AssistantMessageEvent::ToolCallDelta { partial, .. }
                    | AssistantMessageEvent::ToolCallEnd { partial, .. } => partial,
                    _ => unreachable!(),
                };
                expect_no_stream_state(partial);
                if let AssistantMessageEvent::ToolCallDelta { content_index, .. } = other {
                    if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get(*content_index) {
                        if let Some(partial_str) = tc.arguments.get("content").and_then(Value::as_str) {
                            assert!(expected_content.starts_with(partial_str));
                            assert!(partial_str.len() as isize >= shown);
                            shown = partial_str.len() as isize;
                        }
                    }
                }
            }
        }
    }

    let message = s.result().await;
    assert_eq!(message.stop_reason, StopReason::ToolUse, "{:?}", message.error_message);
    expect_no_stream_state(&message);
    assert_eq!(message.content.len(), 2);
    if let AssistantContentBlock::Thinking(th) = &message.content[0] {
        assert_eq!(th.thinking, "[Reasoning redacted]");
        assert_eq!(th.thinking_signature.as_deref(), Some("AQID"));
        assert_eq!(th.redacted, Some(true));
    } else {
        panic!("expected thinking");
    }
    let expected_tc = ToolCall {
        id: "tool_1".to_owned(),
        name: "write".to_owned(),
        namespace: None,
        arguments: serde_json::from_value(args).unwrap(),
        thought_signature: None,
        arguments_error: None,
    };
    assert_eq!(message.content[1], AssistantContentBlock::ToolCall(expected_tc));
}

#[tokio::test]
#[serial]
async fn streams_large_bedrock_tool_call_linearly() {
    let model = test_model();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("write".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    });
    let (args, text) = write_call(250_000);
    let wire = events(&text);
    let _guard = override_bedrock_transport(move |_| {
        let evs = wire.clone();
        Box::pin(async move {
            Ok(BedrockTransportOutcome::Stream(BedrockTransportStreamResponse {
                http_status: Some(200),
                request_id: None,
                headers: indexmap::IndexMap::new(),
                events: evs,
            }))
        })
    });

    let message = stream(&model, &context, None).result().await;
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    if let AssistantContentBlock::ToolCall(tc) = &message.content[1] {
        assert_eq!(tc.arguments.get("content"), args.get("content"));
    } else {
        panic!("expected toolCall");
    }
}

//! Port of packages/ai/test/assistant-message-frame.test.ts (unit ai-api-openai-responses)

use futures::{StreamExt, stream};
use pi_ai::api::openai_responses_shared::{ResponsesEventJsonStream, process_responses_stream};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, JsonObject, Model, ModelCost, ModelInput,
    StopReason, TextContent, ThinkingContent, ToolCall, Usage,
};
use pi_ai::utils::assistant_message_frame::{
    AssistantMessageFrame, AssistantMessageFrameEncoder, reduce_assistant_message_frames,
};
use pi_ai::utils::diagnostics::AssistantMessageDiagnostic;
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::{Value, json};

fn json_obj(val: Value) -> JsonObject {
    let mut map = JsonObject::new();
    if let Value::Object(obj) = val {
        for (k, v) in obj {
            map.insert(k, v);
        }
    }
    map
}

fn seed() -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "test-api".to_owned(),
        provider: "test-provider".to_owned(),
        model: "test-model".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    }
}

fn frame(encoder: &mut AssistantMessageFrameEncoder, event: &AssistantMessageEvent) -> AssistantMessageFrame {
    encoder
        .encode(event)
        .unwrap()
        .expect("expected event to produce a frame")
}

#[test]
fn uses_authoritative_text_end_content_and_signature() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    )];
    partial
        .content
        .push(AssistantContentBlock::Text(TextContent::new("Hello ")));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        },
    ));
    partial.content[0] = AssistantContentBlock::Text(TextContent {
        text: "Hello world".to_owned(),
        text_signature: Some("sig-text".to_owned()),
    });
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "incorrect".to_owned(),
            partial: partial.clone(),
        },
    ));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "Hello world".to_owned(),
            partial,
        },
    ));

    assert_eq!(
        frames.last(),
        Some(&AssistantMessageFrame::TextEnd {
            content_index: 0,
            content: "Hello world".to_owned(),
            text_signature: Some("sig-text".to_owned()),
        })
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![AssistantContentBlock::Text(TextContent {
            text: "Hello world".to_owned(),
            text_signature: Some("sig-text".to_owned()),
        })]
    );
}

#[test]
fn preserves_provider_thinking_level_from_the_stream_start() {
    let mut partial = seed();
    partial.provider_thinking_level = Some("high".to_owned());
    let mut encoder = AssistantMessageFrameEncoder::new();
    let start = frame(&mut encoder, &AssistantMessageEvent::Start { partial });

    assert!(matches!(
        &start,
        AssistantMessageFrame::Start { partial } if partial.provider_thinking_level.as_deref() == Some("high")
    ));
    assert_eq!(
        reduce_assistant_message_frames([start])
            .unwrap()
            .unwrap()
            .provider_thinking_level
            .as_deref(),
        Some("high")
    );
}

#[test]
fn preserves_initial_and_final_thinking_metadata_including_redaction() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    )];
    partial.content.push(AssistantContentBlock::Thinking(ThinkingContent {
        thinking: "[redacted]".to_owned(),
        thinking_signature: Some("encrypted-start".to_owned()),
        redacted: Some(true),
    }));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ThinkingStart {
            content_index: 0,
            partial: partial.clone(),
        },
    ));
    partial.content[0] = AssistantContentBlock::Thinking(ThinkingContent {
        thinking: "[redacted]".to_owned(),
        thinking_signature: Some("encrypted-final".to_owned()),
        redacted: Some(true),
    });
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content: "[redacted]".to_owned(),
            partial,
        },
    ));

    assert_eq!(
        frames.last(),
        Some(&AssistantMessageFrame::ThinkingEnd {
            content_index: 0,
            content: "[redacted]".to_owned(),
            thinking_signature: Some("encrypted-final".to_owned()),
            redacted: Some(true),
        })
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content[0],
        AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "[redacted]".to_owned(),
            thinking_signature: Some("encrypted-final".to_owned()),
            redacted: Some(true),
        })
    );
}

#[test]
fn parses_unfinished_tool_json_once_and_uses_authoritative_completed_arguments() {
    let initial_frames = vec![
        AssistantMessageFrame::Start { partial: seed() },
        AssistantMessageFrame::ToolCallStart {
            content_index: 0,
            tool_call: ToolCall::new("initial-id", "write", JsonObject::new()),
        },
        AssistantMessageFrame::ToolCallDelta {
            content_index: 0,
            delta: r#"{"path":"READ"#.to_owned(),
        },
    ];

    match &reduce_assistant_message_frames(initial_frames.clone())
        .unwrap()
        .unwrap()
        .content[0]
    {
        AssistantContentBlock::ToolCall(tc) => {
            assert_eq!(tc.arguments, json_obj(json!({ "path": "READ" })));
        }
        other => panic!("expected toolCall, got {other:?}"),
    }

    let mut complete_frames = initial_frames;
    complete_frames.push(AssistantMessageFrame::ToolCallDelta {
        content_index: 0,
        delta: r#"ME.md","lines":[1,2]}"#.to_owned(),
    });
    complete_frames.push(AssistantMessageFrame::ToolCallEnd {
        content_index: 0,
        id: "final-id".to_owned(),
        name: "write_file".to_owned(),
        arguments: json_obj(json!({ "path": "final.md", "lines": [3] })),
        thought_signature: Some("thought".to_owned()),
        namespace: Some("files".to_owned()),
    });

    assert_eq!(
        reduce_assistant_message_frames(complete_frames)
            .unwrap()
            .unwrap()
            .content[0],
        AssistantContentBlock::ToolCall(ToolCall {
            id: "final-id".to_owned(),
            name: "write_file".to_owned(),
            arguments: json_obj(json!({ "path": "final.md", "lines": [3] })),
            thought_signature: Some("thought".to_owned()),
            namespace: Some("files".to_owned()),
        })
    );
}

#[tokio::test]
async fn round_trips_openai_responses_content_supplied_only_by_authoritative_end_events() {
    let mut output = seed();
    output.api = "openai-responses".to_owned();
    output.provider = "openai".to_owned();
    let model = Model {
        r#type: None,
        id: output.model.clone(),
        name: "Test".to_owned(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        base_url: "https://api.openai.com/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
    };

    let events = vec![
        Ok(json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "message", "id": "msg", "role": "assistant", "status": "in_progress", "content": [] }
        })),
        Ok(json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "final text", "annotations": [] }]
            }
        })),
        Ok(json!({
            "type": "response.output_item.added",
            "sequence_number": 2,
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc", "call_id": "call", "name": "lookup", "arguments": "" }
        })),
        Ok(json!({
            "type": "response.output_item.done",
            "sequence_number": 3,
            "output_index": 1,
            "item": { "type": "function_call", "id": "fc", "call_id": "call", "name": "lookup", "arguments": r#"{"query":"pi"}"# }
        })),
        Ok(json!({
            "type": "response.completed",
            "sequence_number": 4,
            "response": { "id": "response", "status": "completed", "output": [] }
        })),
    ];
    let source: ResponsesEventJsonStream = Box::pin(stream::iter(events));

    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: output.clone(),
        },
    )];
    let mut event_stream = AssistantMessageEventStream::new();
    let es_for_proc = event_stream.clone();
    process_responses_stream(source, &mut output, &es_for_proc, &model, None)
        .await
        .unwrap();
    event_stream.end(None);
    while let Some(ev) = event_stream.next().await {
        if let Some(f) = encoder.encode(&ev).unwrap() {
            frames.push(f);
        }
    }

    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        output.content
    );
}

#[test]
fn reconciles_queued_text_events_against_one_advanced_live_partial_without_duplicate_content() {
    let mut partial = seed();
    partial.content.push(AssistantContentBlock::Text(TextContent::new("")));
    let mut events = vec![];
    partial.content[0] = AssistantContentBlock::Text(TextContent::new("Hello world"));
    events.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    events.push(AssistantMessageEvent::TextStart {
        content_index: 0,
        partial: partial.clone(),
    });
    for delta in ["Hel", "lo", " ", "world"] {
        events.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: delta.to_owned(),
            partial: partial.clone(),
        });
    }

    let mut encoder = AssistantMessageFrameEncoder::new();
    let frames: Vec<AssistantMessageFrame> = events.iter().filter_map(|e| encoder.encode(e).unwrap()).collect();

    assert_eq!(
        frames.iter().map(|f| f.type_name()).collect::<Vec<_>>(),
        vec!["start", "text_start"]
    );
    match &frames[0] {
        AssistantMessageFrame::Start { partial } => {
            assert!(partial.content.is_empty());
            assert_eq!(partial.stop_reason, StopReason::Pending);
        }
        other => panic!("expected start frame, got {other:?}"),
    }
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![AssistantContentBlock::Text(TextContent::new("Hello world"))]
    );
}

#[test]
fn trims_only_the_covered_prefix_when_a_start_snapshot_lands_inside_a_delta() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    )];
    partial
        .content
        .push(AssistantContentBlock::Text(TextContent::new("Hel")));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        },
    ));
    assert!(
        encoder
            .encode(&AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "He".to_owned(),
                partial: partial.clone(),
            })
            .unwrap()
            .is_none()
    );
    let remainder = encoder
        .encode(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "llo".to_owned(),
            partial,
        })
        .unwrap()
        .expect("expected uncovered text delta");
    frames.push(remainder.clone());

    assert_eq!(
        remainder,
        AssistantMessageFrame::TextDelta {
            content_index: 0,
            delta: "lo".to_owned(),
        }
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![AssistantContentBlock::Text(TextContent::new("Hello"))]
    );
}

#[test]
fn checkpoints_queued_tool_json_without_replaying_covered_deltas() {
    let mut partial = seed();
    partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "write",
        json_obj(json!({ "path": "README.md" })),
    )));
    let events = vec![
        AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
        AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: r#"{"path":"READ"#.to_owned(),
            partial: partial.clone(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: r#"ME.md"}"#.to_owned(),
            partial,
        },
    ];

    let mut encoder = AssistantMessageFrameEncoder::new();
    let frames: Vec<AssistantMessageFrame> = events.iter().filter_map(|e| encoder.encode(e).unwrap()).collect();
    assert_eq!(
        frames.iter().map(|f| f.type_name()).collect::<Vec<_>>(),
        vec!["start", "toolcall_start", "toolcall_checkpoint"]
    );
    assert_eq!(
        frames.last(),
        Some(&AssistantMessageFrame::ToolCallCheckpoint {
            content_index: 0,
            json: r#"{"path":"README.md"}"#.to_owned(),
        })
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "call",
            "write",
            json_obj(json!({ "path": "README.md" })),
        ))]
    );
}

#[test]
fn resumes_legacy_grammar_tool_json_from_initial_arguments() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    )];
    partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "bash",
        json_obj(json!({ "input": "a" })),
    )));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
    ));
    partial.content[0] =
        AssistantContentBlock::ToolCall(ToolCall::new("call", "bash", json_obj(json!({ "input": "ab" }))));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: r#"{"input":"ab"#.to_owned(),
            partial: partial.clone(),
        },
    ));
    partial.content[0] =
        AssistantContentBlock::ToolCall(ToolCall::new("call", "bash", json_obj(json!({ "input": "abc" }))));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: r#"c"}"#.to_owned(),
            partial,
        },
    ));

    assert_eq!(
        frames[2..],
        vec![
            AssistantMessageFrame::ToolCallCheckpoint {
                content_index: 0,
                json: r#"{"input":"ab"#.to_owned(),
            },
            AssistantMessageFrame::ToolCallDelta {
                content_index: 0,
                delta: r#"c"}"#.to_owned(),
            },
        ]
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "call",
            "bash",
            json_obj(json!({ "input": "abc" })),
        ))]
    );
}

#[test]
fn streams_tool_json_compactly_from_an_empty_argument_start() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    let mut frames = vec![frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    )];
    partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "bash",
        JsonObject::new(),
    )));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
    ));
    partial.content[0] = AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "bash",
        json_obj(json!({ "command": "ls -la /tmp" })),
    ));
    frames.push(frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: r#"{"command":"ls -la /tmp"}"#.to_owned(),
            partial,
        },
    ));

    assert_eq!(
        frames.last(),
        Some(&AssistantMessageFrame::ToolCallDelta {
            content_index: 0,
            delta: r#"{"command":"ls -la /tmp"}"#.to_owned(),
        })
    );
    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content[0],
        AssistantContentBlock::ToolCall(ToolCall::new(
            "call",
            "bash",
            json_obj(json!({ "command": "ls -la /tmp" })),
        ))
    );
}

#[test]
fn accepts_a_pre_generation_error_but_rejects_success_or_updates_before_start() {
    let mut failed = seed();
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("setup failed".to_owned());
    assert!(
        AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: failed,
            })
            .unwrap()
            .is_none()
    );

    let mut completed = seed();
    completed.stop_reason = StopReason::Stop;
    assert!(
        AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: completed,
            })
            .unwrap_err()
            .to_string()
            .contains("done event appears before start")
    );
    assert!(
        AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "x".to_owned(),
                partial: seed(),
            })
            .unwrap_err()
            .to_string()
            .contains("text_delta event appears before start")
    );
}

#[test]
fn treats_end_signature_metadata_including_absence_as_authoritative() {
    let frames = vec![
        AssistantMessageFrame::Start { partial: seed() },
        AssistantMessageFrame::TextStart {
            content_index: 0,
            content: TextContent {
                text: String::new(),
                text_signature: Some("stale-text".to_owned()),
            },
        },
        AssistantMessageFrame::TextEnd {
            content_index: 0,
            content: String::new(),
            text_signature: None,
        },
        AssistantMessageFrame::ThinkingStart {
            content_index: 1,
            content: ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some("stale-thinking".to_owned()),
                redacted: Some(true),
            },
        },
        AssistantMessageFrame::ThinkingEnd {
            content_index: 1,
            content: String::new(),
            thinking_signature: Some(String::new()),
            redacted: Some(false),
        },
        AssistantMessageFrame::ToolCallStart {
            content_index: 2,
            tool_call: ToolCall {
                id: "call".to_owned(),
                name: "read".to_owned(),
                arguments: JsonObject::new(),
                thought_signature: Some("stale-tool".to_owned()),
                namespace: Some("stale-namespace".to_owned()),
            },
        },
        AssistantMessageFrame::ToolCallEnd {
            content_index: 2,
            id: "call".to_owned(),
            name: "read".to_owned(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
        },
    ];

    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![
            AssistantContentBlock::Text(TextContent::new("")),
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: String::new(),
                thinking_signature: Some(String::new()),
                redacted: Some(false),
            }),
            AssistantContentBlock::ToolCall(ToolCall::new("call", "read", JsonObject::new())),
        ]
    );
}

#[test]
fn stores_authoritative_final_arguments_in_toolcall_end_frames() {
    let mut partial = seed();
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read".to_owned(),
        arguments: json_obj(json!({ "path": "README.md" })),
        thought_signature: Some("thought".to_owned()),
        namespace: Some("files".to_owned()),
    };
    partial.content.push(AssistantContentBlock::ToolCall(tool_call.clone()));

    let mut encoder = AssistantMessageFrameEncoder::new();
    frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    );
    frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
    );
    let end = frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallEnd {
            content_index: 0,
            tool_call,
            partial,
        },
    );
    assert_eq!(
        end,
        AssistantMessageFrame::ToolCallEnd {
            content_index: 0,
            id: "call-1".to_owned(),
            name: "read".to_owned(),
            arguments: json_obj(json!({ "path": "README.md" })),
            thought_signature: Some("thought".to_owned()),
            namespace: Some("files".to_owned()),
        }
    );
}

#[test]
fn supports_interleaved_streams_by_content_index() {
    let frames = vec![
        AssistantMessageFrame::Start { partial: seed() },
        AssistantMessageFrame::TextStart {
            content_index: 0,
            content: TextContent::new(""),
        },
        AssistantMessageFrame::ToolCallStart {
            content_index: 1,
            tool_call: ToolCall::new("call", "lookup", JsonObject::new()),
        },
        AssistantMessageFrame::ThinkingStart {
            content_index: 2,
            content: ThinkingContent::new(""),
        },
        AssistantMessageFrame::TextDelta {
            content_index: 0,
            delta: "answer".to_owned(),
        },
        AssistantMessageFrame::ToolCallDelta {
            content_index: 1,
            delta: r#"{"query":"pi"}"#.to_owned(),
        },
        AssistantMessageFrame::ThinkingDelta {
            content_index: 2,
            delta: "check".to_owned(),
        },
        AssistantMessageFrame::ToolCallEnd {
            content_index: 1,
            id: "call".to_owned(),
            name: "lookup".to_owned(),
            arguments: json_obj(json!({ "query": "pi" })),
            thought_signature: None,
            namespace: None,
        },
        AssistantMessageFrame::TextEnd {
            content_index: 0,
            content: "answer".to_owned(),
            text_signature: None,
        },
        AssistantMessageFrame::ThinkingEnd {
            content_index: 2,
            content: "check".to_owned(),
            thinking_signature: None,
            redacted: None,
        },
    ];

    assert_eq!(
        reduce_assistant_message_frames(frames).unwrap().unwrap().content,
        vec![
            AssistantContentBlock::Text(TextContent::new("answer")),
            AssistantContentBlock::ToolCall(ToolCall::new("call", "lookup", json_obj(json!({ "query": "pi" })),)),
            AssistantContentBlock::Thinking(ThinkingContent::new("check")),
        ]
    );
}

#[test]
fn snapshots_mutable_event_data_and_keeps_reduction_pure() {
    let mut partial = seed();
    partial.diagnostics = Some(vec![AssistantMessageDiagnostic {
        r#type: "test".to_owned(),
        timestamp: 2,
        error: None,
        details: Some(json_obj(json!({ "value": "original" }))),
    }]);
    let mut encoder = AssistantMessageFrameEncoder::new();
    let start = frame(
        &mut encoder,
        &AssistantMessageEvent::Start {
            partial: partial.clone(),
        },
    );
    partial.diagnostics.as_mut().unwrap()[0].details = Some(json_obj(json!({ "value": "mutated" })));
    partial.usage.cost.total = 99.0;

    partial.content.push(AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "run",
        json_obj(json!({ "nested": { "value": "original" } })),
    )));
    let tool_start = frame(
        &mut encoder,
        &AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: partial.clone(),
        },
    );
    partial.content[0] = AssistantContentBlock::ToolCall(ToolCall::new(
        "call",
        "run",
        json_obj(json!({ "nested": { "value": "mutated" } })),
    ));

    let mut reduced = reduce_assistant_message_frames([start, tool_start.clone()])
        .unwrap()
        .unwrap();
    assert_eq!(
        reduced.diagnostics.as_ref().unwrap()[0]
            .details
            .as_ref()
            .unwrap()
            .get("value"),
        Some(&json!("original"))
    );
    assert_eq!(reduced.usage.cost.total, 0.0);
    match &mut reduced.content[0] {
        AssistantContentBlock::ToolCall(tc) => {
            assert_eq!(tc.arguments.get("nested"), Some(&json!({ "value": "original" })));
            tc.arguments.insert("nested".to_owned(), json!("changed-output"));
        }
        other => panic!("expected toolCall, got {other:?}"),
    }
    match tool_start {
        AssistantMessageFrame::ToolCallStart { tool_call, .. } => {
            assert_eq!(tool_call.arguments.get("nested"), Some(&json!({ "value": "original" })));
        }
        _ => unreachable!(),
    }
}

#[test]
fn omits_terminal_events_because_settlement_is_separate() {
    let mut message = seed();
    let mut completed = AssistantMessageFrameEncoder::new();
    completed
        .encode(&AssistantMessageEvent::Start {
            partial: message.clone(),
        })
        .unwrap();
    message.stop_reason = StopReason::Stop;
    assert!(
        completed
            .encode(&AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: message.clone(),
            })
            .unwrap()
            .is_none()
    );
    message.stop_reason = StopReason::Error;
    message.error_message = Some("failed".to_owned());
    assert!(
        AssistantMessageFrameEncoder::new()
            .encode(&AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: message,
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn returns_none_when_there_is_no_start_frame() {
    assert!(
        reduce_assistant_message_frames(Vec::<AssistantMessageFrame>::new())
            .unwrap()
            .is_none()
    );
    assert!(
        reduce_assistant_message_frames([AssistantMessageFrame::TextDelta {
            content_index: 0,
            delta: "x".to_owned(),
        }])
        .unwrap()
        .is_none()
    );
}

#[test]
fn rejects_frames_before_start_wrong_block_kinds_duplicate_ends_and_index_gaps() {
    assert!(
        reduce_assistant_message_frames([
            AssistantMessageFrame::TextDelta {
                content_index: 0,
                delta: "x".to_owned(),
            },
            AssistantMessageFrame::Start { partial: seed() },
        ])
        .unwrap_err()
        .to_string()
        .contains("before the start frame")
    );
    assert!(
        reduce_assistant_message_frames([
            AssistantMessageFrame::Start { partial: seed() },
            AssistantMessageFrame::ToolCallStart {
                content_index: 0,
                tool_call: ToolCall::new("call", "run", JsonObject::new()),
            },
            AssistantMessageFrame::TextDelta {
                content_index: 0,
                delta: "wrong".to_owned(),
            },
        ])
        .unwrap_err()
        .to_string()
        .contains("expected text block")
    );
    assert!(
        reduce_assistant_message_frames([
            AssistantMessageFrame::Start { partial: seed() },
            AssistantMessageFrame::TextStart {
                content_index: 0,
                content: TextContent::new(""),
            },
            AssistantMessageFrame::TextEnd {
                content_index: 0,
                content: String::new(),
                text_signature: None,
            },
            AssistantMessageFrame::TextEnd {
                content_index: 0,
                content: String::new(),
                text_signature: None,
            },
        ])
        .unwrap_err()
        .to_string()
        .contains("follows the end")
    );
    assert!(
        reduce_assistant_message_frames([
            AssistantMessageFrame::Start { partial: seed() },
            AssistantMessageFrame::TextStart {
                content_index: 1,
                content: TextContent::new(""),
            },
        ])
        .unwrap_err()
        .to_string()
        .contains("would leave a gap")
    );
}

#[test]
fn rejects_conversion_events_whose_content_index_points_to_the_wrong_block_kind() {
    let mut partial = seed();
    let mut encoder = AssistantMessageFrameEncoder::new();
    encoder
        .encode(&AssistantMessageEvent::Start {
            partial: partial.clone(),
        })
        .unwrap();
    partial
        .content
        .push(AssistantContentBlock::Thinking(ThinkingContent::new("")));
    assert!(
        encoder
            .encode(&AssistantMessageEvent::TextStart {
                content_index: 0,
                partial,
            })
            .unwrap_err()
            .to_string()
            .contains("text_start event points to thinking block")
    );
}

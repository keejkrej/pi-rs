//! Port of packages/ai/test/streaming-tool-arguments.test.ts (#9265)

use std::sync::Arc;
use std::time::Instant;

use futures::{StreamExt, stream};
use pi_ai::AssistantMessageEventStream;
use pi_ai::api::anthropic_messages;
use pi_ai::api::mistral_conversations;
use pi_ai::api::openai_completions;
use pi_ai::api::openai_responses_shared::{
    ProcessResponsesStreamOptions, ResponsesEventJsonStream, process_responses_stream,
};
use pi_ai::api::pi_messages;
use pi_ai::compat::normalize_context;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_ai::types::{
    Api, ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, JsonObject,
    KnownApi, KnownProvider, Message, Model, ModelCost, ModelInput, ProviderId, StopReason, StreamOptions,
    TranscriptContext, Usage, UsageCost, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use serde_json::{Value, json};

fn base_content() -> String {
    (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
        .collect()
}

fn write_call(content: &str, size: usize) -> (JsonObject, String) {
    let mut args = JsonObject::new();
    args.insert("path".to_owned(), json!("/src/file.ts"));
    args.insert("content".to_owned(), json!(&content[..size]));
    let text = pi_js::json::stringify(&Value::Object(args.clone().into_iter().collect()));
    (args, text)
}

fn deltas_of(text: &str, size: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let end = (i + size).min(text.len());
        out.push(text[i..end].to_owned());
        i = end;
    }
    out
}

fn sse_line(data: &Value, event: Option<&str>) -> String {
    let data_str = pi_js::json::stringify(data);
    match event {
        Some(ev) => format!("event: {ev}\ndata: {data_str}\n\n"),
        None => format!("data: {data_str}\n\n"),
    }
}

fn sse_fetch(events: Vec<String>) -> pi_ai::types::FetchFunction {
    let chunks: Vec<String> = events.chunks(16).map(|batch| batch.concat()).collect();
    Arc::new(move |_req: Request| {
        let batches = chunks.clone();
        Box::pin(async move {
            let body = stream::iter(batches.into_iter().map(|chunk| Ok(chunk.into_bytes().into())));
            Ok(Response::from_stream(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                body,
            ))
        })
    })
}

fn base_model(id: &str, name: &str, api: Api, provider: ProviderId, base_url: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: name.to_owned(),
        api,
        provider,
        base_url: base_url.to_owned(),
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

fn test_context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("write".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    })
}

fn anthropic_events(deltas: &[String]) -> Vec<String> {
    let mut out = vec![
        sse_line(
            &json!({
                "type": "message_start",
                "message": { "id": "msg_1", "model": "claude-test", "usage": { "input_tokens": 1, "output_tokens": 1 } },
            }),
            Some("message_start"),
        ),
        sse_line(
            &json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": "write", "input": {} },
            }),
            Some("content_block_start"),
        ),
    ];
    for delta in deltas {
        out.push(sse_line(
            &json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": delta },
            }),
            Some("content_block_delta"),
        ));
    }
    out.push(sse_line(
        &json!({ "type": "content_block_stop", "index": 0 }),
        Some("content_block_stop"),
    ));
    out.push(sse_line(
        &json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 10 } }),
        Some("message_delta"),
    ));
    out.push(sse_line(&json!({ "type": "message_stop" }), Some("message_stop")));
    out
}

fn chat_events(deltas: &[String], with_id_every_chunk: bool) -> Vec<String> {
    let call_chunk = |fields: Value| {
        sse_line(
            &json!({
                "id": "chat_1",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": "test",
                "choices": [{ "index": 0, "delta": { "tool_calls": [fields] } }],
            }),
            None,
        )
    };
    let mut out = vec![call_chunk(json!({
        "index": 0,
        "id": "call_1",
        "type": "function",
        "function": { "name": "write", "arguments": "" },
    }))];
    for delta in deltas {
        out.push(call_chunk(if with_id_every_chunk {
            json!({ "index": 0, "id": "call_1", "function": { "name": "write", "arguments": delta } })
        } else {
            json!({ "index": 0, "function": { "arguments": delta } })
        }));
    }
    out.push(sse_line(
        &json!({
            "id": "chat_1",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "test",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
        }),
        None,
    ));
    out.push("data: [DONE]\n\n".to_owned());
    out
}

fn pi_messages_events(deltas: &[String], args: &JsonObject) -> Vec<String> {
    let mut out = vec![
        sse_line(&json!({ "type": "start" }), None),
        sse_line(
            &json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "write" }),
            None,
        ),
    ];
    for delta in deltas {
        out.push(sse_line(
            &json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": delta }),
            None,
        ));
    }
    out.push(sse_line(
        &json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": { "type": "toolCall", "id": "call_1", "name": "write", "arguments": args },
        }),
        None,
    ));
    out.push(sse_line(
        &json!({
            "type": "done",
            "reason": "toolUse",
            "usage": {
                "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
            },
        }),
        None,
    ));
    out
}

fn responses_events(deltas: &[String], text: &str) -> ResponsesEventJsonStream {
    let item = json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "write",
        "arguments": "",
        "status": "in_progress",
    });
    let mut events = vec![json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "sequence_number": 0,
        "item": item,
    })];
    for delta in deltas {
        events.push(json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "item_id": "fc_1",
            "sequence_number": 0,
            "delta": delta,
        }));
    }
    events.push(json!({
        "type": "response.function_call_arguments.done",
        "output_index": 0,
        "item_id": "fc_1",
        "sequence_number": 0,
        "arguments": text,
    }));
    let mut completed_item = item;
    completed_item["arguments"] = Value::String(text.to_owned());
    completed_item["status"] = json!("completed");
    events.push(json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "sequence_number": 0,
        "item": completed_item,
    }));
    events.push(json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_1",
            "status": "completed",
            "output": [],
            "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 },
        },
    }));
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[derive(Clone, Copy)]
enum PathKind {
    AnthropicMessages,
    OpenAICompletions,
    OpenAIResponses,
    MistralConversations,
    PiMessages,
    Faux,
}

impl PathKind {
    fn name(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAICompletions => "openai-completions",
            Self::OpenAIResponses => "openai-responses",
            Self::MistralConversations => "mistral-conversations",
            Self::PiMessages => "pi-messages",
            Self::Faux => "faux",
        }
    }

    fn start(self, deltas: &[String], args: &JsonObject, text: &str) -> AssistantMessageEventStream {
        let ctx = test_context();
        match self {
            Self::AnthropicMessages => anthropic_messages::stream(
                &base_model(
                    "claude-test",
                    "c",
                    Api::from(KnownApi::AnthropicMessages),
                    ProviderId::from(KnownProvider::Anthropic),
                    "https://api.anthropic.com",
                ),
                &ctx,
                Some(ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some("sk-ant-test".to_owned()),
                        fetch: Some(sse_fetch(anthropic_events(deltas))),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            ),
            Self::OpenAICompletions => openai_completions::stream(
                &base_model(
                    "gpt-test",
                    "g",
                    Api::from(KnownApi::OpenAICompletions),
                    ProviderId::from(KnownProvider::Openai),
                    "https://api.openai.com/v1",
                ),
                &ctx,
                Some(ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some("sk-test".to_owned()),
                        fetch: Some(sse_fetch(chat_events(deltas, false))),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            ),
            Self::OpenAIResponses => {
                let model = base_model(
                    "gpt-test",
                    "g",
                    Api::from(KnownApi::OpenAIResponses),
                    ProviderId::from(KnownProvider::Openai),
                    "https://api.openai.com/v1",
                );
                let mut output = AssistantMessage {
                    content: Vec::new(),
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    response_model: None,
                    response_id: None,
                    provider_thinking_level: None,
                    thinking_level: None,
                    diagnostics: None,
                    usage: Usage {
                        input: 0,
                        output: 0,
                        cache_read: 0,
                        cache_write: 0,
                        cache_write_1h: None,
                        reasoning: None,
                        total_tokens: 0,
                        cost: UsageCost::default(),
                    },
                    stop_reason: StopReason::Pending,
                    deferred: None,
                    error_message: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: 0,
                    duration_ms: None,
                };
                let events = AssistantMessageEventStream::new();
                let events_task = events.clone();
                let ev_stream = responses_events(deltas, text);
                tokio::spawn(async move {
                    events_task.push(AssistantMessageEvent::Start {
                        partial: output.clone(),
                    });
                    let _ = process_responses_stream(
                        ev_stream,
                        &mut output,
                        &events_task,
                        &model,
                        Some(&ProcessResponsesStreamOptions::default()),
                    )
                    .await;
                    events_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: output.clone(),
                    });
                    events_task.end(Some(output));
                });
                events
            }
            Self::MistralConversations => mistral_conversations::stream(
                &base_model(
                    "devstral",
                    "d",
                    Api::from(KnownApi::MistralConversations),
                    ProviderId::from(KnownProvider::Mistral),
                    "https://api.mistral.ai",
                ),
                &ctx,
                Some(ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some("test".to_owned()),
                        fetch: Some(sse_fetch(chat_events(deltas, true))),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            ),
            Self::PiMessages => pi_messages::stream(
                &base_model(
                    "auto",
                    "a",
                    Api::from(KnownApi::PiMessages),
                    ProviderId::from("radius"),
                    "https://example.test/v1",
                ),
                &ctx,
                Some(ApiStreamOptions {
                    base: StreamOptions {
                        api_key: Some("test".to_owned()),
                        fetch: Some(sse_fetch(pi_messages_events(deltas, args))),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            ),
            Self::Faux => {
                let faux = faux_provider(Some(RegisterFauxProviderOptions {
                    token_size: Some(FauxTokenSize {
                        min: Some(8),
                        max: Some(8),
                    }),
                    ..Default::default()
                }));
                faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
                    FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                        "write",
                        args.clone(),
                        Some(FauxToolCallOptions {
                            id: Some("call_1".to_owned()),
                        }),
                    ))]),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::ToolUse),
                        ..Default::default()
                    }),
                ))]);
                let model = faux.get_model();
                faux.provider().stream_simple(&model, &ctx, None)
            }
        }
    }
}

fn event_message(ev: &AssistantMessageEvent) -> &AssistantMessage {
    match ev {
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
        AssistantMessageEvent::Done { message, .. } => message,
        AssistantMessageEvent::Error { error, .. } => error,
    }
}

fn expect_no_stream_state(message: &AssistantMessage, label: &str) {
    let serialized = serde_json::to_value(message).unwrap();
    for block in serialized["content"].as_array().unwrap() {
        let obj = block.as_object().unwrap();
        let allowed: &[&str] = match obj["type"].as_str().unwrap_or("") {
            "text" => &["type", "text", "textSignature"],
            "thinking" => &["type", "thinking", "thinkingSignature", "redacted"],
            "toolCall" => &[
                "type",
                "id",
                "name",
                "arguments",
                "argumentsError",
                "thoughtSignature",
                "namespace",
            ],
            other => panic!("{label}: unexpected block type {other}"),
        };
        for key in obj.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{label}: unexpected stream state key {key}"
            );
        }
    }
}

#[tokio::test]
async fn streaming_tool_call_arguments_across_all_six_paths() {
    let content = base_content();
    let paths = [
        PathKind::AnthropicMessages,
        PathKind::OpenAICompletions,
        PathKind::OpenAIResponses,
        PathKind::MistralConversations,
        PathKind::PiMessages,
        PathKind::Faux,
    ];

    // 1. Growing partial arguments, no stream state leaked, exact final arguments.
    let (args_small, text_small) = write_call(&content, 5_000);
    let expected_content = args_small.get("content").and_then(Value::as_str).unwrap().to_owned();
    let deltas = deltas_of(&text_small, 24);
    for path in paths {
        let mut s = path.start(&deltas, &args_small, &text_small);
        let mut shown = 0usize;
        let mut partials = 0usize;
        while let Some(ev) = s.next().await {
            let partial = event_message(&ev);
            expect_no_stream_state(partial, path.name());
            if let AssistantMessageEvent::ToolCallDelta {
                content_index, partial, ..
            } = &ev
            {
                if let Some(AssistantContentBlock::ToolCall(tc)) = partial.content.get(*content_index) {
                    if let Some(shown_str) = tc.arguments.get("content").and_then(Value::as_str) {
                        partials += 1;
                        assert!(
                            expected_content.starts_with(shown_str),
                            "{}: partial is not a prefix",
                            path.name()
                        );
                        assert!(shown_str.len() >= shown, "{}: partial shrank", path.name());
                        shown = shown_str.len();
                    }
                }
            }
        }
        let message = s.result().await;
        assert_eq!(message.stop_reason, StopReason::ToolUse, "{}", path.name());
        assert!(partials > 100, "{}: only {partials} partials", path.name());
        assert_eq!(message.content.len(), 1);
        match &message.content[0] {
            AssistantContentBlock::ToolCall(tc) => {
                assert_eq!(tc.name, "write");
                assert_eq!(tc.arguments, args_small);
            }
            other => panic!("{}: expected ToolCall, got {other:?}", path.name()),
        }
    }

    // 2. Linear scaling in the argument text across 10x input growth.
    // PORT: Measure elapsed time scaling across 25k vs 250k inputs in place of V8 heap stats.
    let (args_25k, text_25k) = write_call(&content, 25_000);
    let deltas_25k = deltas_of(&text_25k, 32);
    let (args_250k, text_250k) = write_call(&content, 250_000);
    let deltas_250k = deltas_of(&text_250k, 32);

    for path in paths {
        let t1 = Instant::now();
        let mut s1 = path.start(&deltas_25k, &args_25k, &text_25k);
        while s1.next().await.is_some() {}
        let r1 = s1.result().await;
        assert_eq!(r1.stop_reason, StopReason::ToolUse);
        let ns1 = t1.elapsed().as_nanos().max(1);

        let t2 = Instant::now();
        let mut s2 = path.start(&deltas_250k, &args_250k, &text_250k);
        while s2.next().await.is_some() {}
        let r2 = s2.result().await;
        assert_eq!(r2.stop_reason, StopReason::ToolUse);
        let ns2 = t2.elapsed().as_nanos().max(1);

        let ratio = (ns2 as f64) / (ns1 as f64);
        assert!(
            ratio < 100.0 && (ns2 as f64) < 10e9,
            "{}: 10x growth took {ratio:.2}x (25k={ns1}ns, 250k={ns2}ns)",
            path.name()
        );
    }
}

#[tokio::test]
async fn leaves_no_stream_state_on_error_message_when_cut_off_mid_call() {
    let content = base_content();
    let (_, text) = write_call(&content, 2_000);
    let cut = deltas_of(&text, 16).into_iter().take(20).collect::<Vec<_>>();
    let mut events = anthropic_events(&cut);
    let keep = events.len().saturating_sub(3);
    events.truncate(keep);

    let mut s = anthropic_messages::stream(
        &base_model(
            "claude-test",
            "c",
            Api::from(KnownApi::AnthropicMessages),
            ProviderId::from(KnownProvider::Anthropic),
            "https://api.anthropic.com",
        ),
        &test_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("sk-ant-test".to_owned()),
                fetch: Some(sse_fetch(events)),
                ..Default::default()
            },
            ..Default::default()
        }),
    );
    while s.next().await.is_some() {}
    let message = s.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    expect_no_stream_state(&message, "error");
    match &message.content[0] {
        AssistantContentBlock::ToolCall(tc) => {
            assert_eq!(tc.arguments.get("path"), Some(&json!("/src/file.ts")));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

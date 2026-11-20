//! Port of packages/ai/test/faux-provider.test.ts

use futures::StreamExt;
use pi_ai::compat::{complete, register_faux_provider, stream};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition,
    FauxProviderRegistration, FauxResponseFactory, FauxResponseStep, FauxTokenSize, FauxToolCallOptions,
    RegisterFauxProviderOptions, faux_assistant_message, faux_text, faux_thinking, faux_tool_call,
};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, Context, ImageContent, JsonObject,
    Message, StopReason, StreamOptions, TextContent, ThinkingContent, Tool, ToolResultMessage, UserContentBlock,
    UserMessage, UserMessageContent,
};
use pi_js::abort::AbortController;
use pi_js::vendor::typebox::t;
use pi_js::{Error, time};
use serde_json::{Value, json};
use serial_test::serial;

struct AutoUnregister(Vec<FauxProviderRegistration>);

impl AutoUnregister {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, reg: FauxProviderRegistration) {
        self.0.push(reg);
    }
}

impl Drop for AutoUnregister {
    fn drop(&mut self) {
        for reg in self.0.drain(..) {
            reg.unregister();
        }
    }
}

async fn collect_events(mut stream_result: pi_ai::AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream_result.next().await {
        events.push(event);
    }
    events
}

fn event_type_name(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

fn json_obj(val: Value) -> JsonObject {
    let mut map = JsonObject::new();
    if let Value::Object(obj) = val {
        for (k, v) in obj {
            map.insert(k, v);
        }
    }
    map
}

fn user_msg(text: &str, ts: i64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: ts,
    })
}

fn model_def(id: &str) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.to_owned(),
        name: None,
        reasoning: None,
        input: None,
        input_limits: None,
        cost: None,
        context_window: None,
        max_tokens: None,
    }
}

#[tokio::test]
#[serial]
async fn registers_a_custom_provider_and_estimates_usage() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello world".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    let state = reg.state();
    regs.push(reg);

    let context = Context {
        system_prompt: Some("Be concise.".to_owned()),
        messages: vec![user_msg("hi there", time::now_ms())],
        tools: None,
    };

    let response = complete(&model, &context, None).await;
    assert_eq!(
        response.content,
        vec![AssistantContentBlock::Text(TextContent::new("hello world"))]
    );
    assert!(response.usage.input > 0);
    assert!(response.usage.output > 0);
    assert_eq!(
        response.usage.total_tokens,
        response.usage.input + response.usage.output
    );
    assert_eq!(state.call_count(), 1);
}

#[tokio::test]
#[serial]
async fn supports_helper_blocks_for_text_thinking_and_tool_calls() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Thinking(faux_thinking("think")),
            FauxContentBlock::ToolCall(faux_tool_call("echo", json_obj(json!({ "text": "hi" })), None)),
            FauxContentBlock::Text(faux_text("done")),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let response = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;

    assert_eq!(response.content.len(), 3);
    assert_eq!(
        response.content[0],
        AssistantContentBlock::Thinking(ThinkingContent::new("think"))
    );
    match &response.content[1] {
        AssistantContentBlock::ToolCall(tc) => {
            assert!(!tc.id.is_empty());
            assert_eq!(tc.name, "echo");
            assert_eq!(tc.arguments, json_obj(json!({ "text": "hi" })));
        }
        other => panic!("expected toolCall block, got {other:?}"),
    }
    assert_eq!(
        response.content[2],
        AssistantContentBlock::Text(TextContent::new("done"))
    );
    assert_eq!(response.stop_reason, StopReason::ToolUse);
}

#[tokio::test]
#[serial]
async fn supports_multiple_models_with_per_model_reasoning_and_model_aware_factories() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                name: Some("Faux Fast".to_owned()),
                reasoning: Some(false),
                ..model_def("faux-fast")
            },
            FauxModelDefinition {
                name: Some("Faux Thinker".to_owned()),
                reasoning: Some(true),
                ..model_def("faux-thinker")
            },
        ]),
        ..Default::default()
    }));
    let factory: FauxResponseFactory = std::sync::Arc::new(|_ctx, _opts, _state, model| {
        Box::pin(async move {
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Text(format!("{}:{}", model.id, model.reasoning)),
                None,
            ))
        })
    });
    reg.set_responses(vec![
        FauxResponseStep::Factory(factory.clone()),
        FauxResponseStep::Factory(factory),
    ]);

    let ids: Vec<String> = reg.models().iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, vec!["faux-fast", "faux-thinker"]);
    assert_eq!(reg.get_model(), reg.models()[0]);
    assert_eq!(reg.get_model_model_id("faux-fast").map(|m| m.reasoning), Some(false));
    assert_eq!(reg.get_model_model_id("faux-thinker").map(|m| m.reasoning), Some(true));

    let fast_model = reg.get_model_model_id("faux-fast").unwrap();
    let thinker_model = reg.get_model_model_id("faux-thinker").unwrap();
    regs.push(reg);

    let fast = complete(
        &fast_model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;
    let thinker = complete(
        &thinker_model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;

    assert_eq!(
        fast.content,
        vec![AssistantContentBlock::Text(TextContent::new("faux-fast:false"))]
    );
    assert_eq!(
        thinker.content,
        vec![AssistantContentBlock::Text(TextContent::new("faux-thinker:true"))]
    );
}

#[tokio::test]
#[serial]
async fn rewrites_api_provider_and_model_on_returned_messages() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        api: Some("faux:test".to_owned()),
        provider: Some("faux-provider".to_owned()),
        models: Some(vec![model_def("faux-model")]),
        ..Default::default()
    }));
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let response = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;

    assert_eq!(response.api, "faux:test");
    assert_eq!(response.provider, "faux-provider");
    assert_eq!(response.model, "faux-model");
}

#[tokio::test]
#[serial]
async fn consumes_queued_responses_in_order_and_errors_when_exhausted() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("second".to_owned()),
            None,
        )),
    ]);
    let model = reg.get_model();

    let context = Context {
        system_prompt: None,
        messages: vec![user_msg("hi", time::now_ms())],
        tools: None,
    };

    let first = complete(&model, &context, None).await;
    let second = complete(&model, &context, None).await;
    let exhausted = complete(&model, &context, None).await;

    assert_eq!(
        first.content,
        vec![AssistantContentBlock::Text(TextContent::new("first"))]
    );
    assert_eq!(
        second.content,
        vec![AssistantContentBlock::Text(TextContent::new("second"))]
    );
    assert_eq!(exhausted.stop_reason, StopReason::Error);
    assert_eq!(
        exhausted.error_message.as_deref(),
        Some("No more faux responses queued")
    );
    assert_eq!(reg.get_pending_response_count(), 0);
    assert_eq!(reg.state().call_count(), 3);
    regs.push(reg);
}

#[tokio::test]
#[serial]
async fn can_replace_and_append_queued_responses() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("first".to_owned()),
        None,
    ))]);
    let model = reg.get_model();

    let context = Context {
        system_prompt: None,
        messages: vec![user_msg("hi", time::now_ms())],
        tools: None,
    };

    assert_eq!(
        complete(&model, &context, None).await.content,
        vec![AssistantContentBlock::Text(TextContent::new("first"))]
    );
    assert_eq!(reg.get_pending_response_count(), 0);

    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("second".to_owned()),
        None,
    ))]);
    assert_eq!(reg.get_pending_response_count(), 1);
    assert_eq!(
        complete(&model, &context, None).await.content,
        vec![AssistantContentBlock::Text(TextContent::new("second"))]
    );

    reg.append_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("third".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("fourth".to_owned()),
            None,
        )),
    ]);
    assert_eq!(reg.get_pending_response_count(), 2);
    assert_eq!(
        complete(&model, &context, None).await.content,
        vec![AssistantContentBlock::Text(TextContent::new("third"))]
    );
    assert_eq!(
        complete(&model, &context, None).await.content,
        vec![AssistantContentBlock::Text(TextContent::new("fourth"))]
    );
    assert_eq!(reg.get_pending_response_count(), 0);
    regs.push(reg);
}

#[tokio::test]
#[serial]
async fn supports_async_response_factories() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Factory(std::sync::Arc::new(
        |context, _opts, state, _model| {
            Box::pin(async move {
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(format!("{}:{}", context.messages.len(), state.call_count())),
                    None,
                ))
            })
        },
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let response = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;

    assert_eq!(
        response.content,
        vec![AssistantContentBlock::Text(TextContent::new("1:1"))]
    );
}

#[tokio::test]
#[serial]
async fn emits_an_error_when_a_response_factory_throws() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Factory(std::sync::Arc::new(
        |_ctx, _opts, _state, _model| Box::pin(async { Err(Error::msg("boom")) }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    assert_eq!(events.len(), 1);
    match &events[0] {
        AssistantMessageEvent::Error { error, .. } => {
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(error.error_message.as_deref(), Some("boom"));
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn rejects_a_queued_response_without_a_terminal_stop_reason() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("partial".to_owned()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Pending),
            ..Default::default()
        }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    assert!(!events.iter().any(|e| matches!(e, AssistantMessageEvent::Done { .. })));
    match events.last() {
        Some(AssistantMessageEvent::Error { error, .. }) => {
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(
                error.error_message.as_deref(),
                Some("Faux response ended without a stop reason")
            );
        }
        other => panic!("expected terminal error event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn estimates_prompt_and_output_tokens_from_serialized_context() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let tool = Tool {
        name: "echo".to_owned(),
        description: "Echo back text".to_owned(),
        parameters: t::object([("text", t::string())]),
        constrained_sampling: None,
    };
    let context = Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Blocks(vec![
                    UserContentBlock::Text(TextContent::new("hello")),
                    UserContentBlock::Image(ImageContent::new("abcd", "image/png")),
                ]),
                timestamp: 1,
            }),
            Message::Assistant(faux_assistant_message(
                FauxAssistantMessageContent::Text("prior".to_owned()),
                None,
            )),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "tool-1".to_owned(),
                tool_name: "echo".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("tool out"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            }),
        ],
        tools: Some(vec![tool.clone()]),
    };

    let response = complete(&model, &context, None).await;
    let tools_json = pi_js::json::stringify(&serde_json::to_value(&vec![tool]).unwrap());
    let prompt_text = [
        "system:sys".to_owned(),
        "user:hello\n[image:image/png:4]".to_owned(),
        "assistant:prior".to_owned(),
        "toolResult:echo\ntool out".to_owned(),
        format!("tools:{tools_json}"),
    ]
    .join("\n\n");
    let expected_prompt_tokens = ((prompt_text.len() + 3) / 4) as i64;
    let expected_output_tokens = (("done".len() + 3) / 4) as i64;

    assert_eq!(response.usage.input, expected_prompt_tokens);
    assert_eq!(response.usage.output, expected_output_tokens);
    assert_eq!(response.usage.cache_read, 0);
    assert_eq!(response.usage.cache_write, 0);
    assert_eq!(
        response.usage.total_tokens,
        expected_prompt_tokens + expected_output_tokens
    );
}

#[tokio::test]
#[serial]
async fn does_not_share_cache_across_sessions_or_requests_without_session_id() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("second".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("third".to_owned()),
            None,
        )),
    ]);
    let model = reg.get_model();
    regs.push(reg);

    let mut context = Context {
        system_prompt: None,
        messages: vec![user_msg("hello", time::now_ms())],
        tools: None,
    };

    let first = complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::Short),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    assert!(first.usage.cache_write > 0);
    context.messages.push(Message::Assistant(first));
    context.messages.push(user_msg("follow up", time::now_ms() + 1));

    let second = complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-2".to_owned()),
                cache_retention: Some(CacheRetention::Short),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    assert_eq!(second.usage.cache_read, 0);
    assert!(second.usage.cache_write > 0);

    let third = complete(&model, &context, None).await;
    assert_eq!(third.usage.cache_read, 0);
    assert_eq!(third.usage.cache_write, 0);
}

#[tokio::test]
#[serial]
async fn simulates_prompt_caching_per_session_id() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("second".to_owned()),
            None,
        )),
    ]);
    let model = reg.get_model();
    regs.push(reg);

    let mut context = Context {
        system_prompt: Some("Be concise.".to_owned()),
        messages: vec![user_msg("hello", time::now_ms())],
        tools: None,
    };

    let first = complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::Short),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    assert_eq!(first.usage.cache_read, 0);
    assert!(first.usage.cache_write > 0);

    context.messages.push(Message::Assistant(first));
    context.messages.push(user_msg("follow up", time::now_ms() + 1));

    let second = complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::Short),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    assert!(second.usage.cache_read > 0);
    assert!(second.usage.input + second.usage.cache_read > second.usage.input);
}

#[tokio::test]
#[serial]
async fn counts_cached_characters_up_to_the_first_difference_in_the_joined_prompt() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("a".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("b".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("c".to_owned()),
            None,
        )),
    ]);
    let model = reg.get_model();
    regs.push(reg);

    let opts = || {
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::Short),
                ..Default::default()
            }
            .into(),
        )
    };

    // Prompt texts: "user:hello world" (16 characters), then 16 + 2 + "user:next" (9) = 27.
    let first = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hello world", 1)],
            tools: None,
        },
        opts(),
    )
    .await;
    assert_eq!(first.usage.input, 0);
    assert_eq!(first.usage.cache_read, 0);
    assert_eq!(first.usage.cache_write, 4);

    let extended = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hello world", 1), user_msg("next", 1)],
            tools: None,
        },
        opts(),
    )
    .await;
    assert_eq!(extended.usage.input, 0);
    assert_eq!(extended.usage.cache_read, 4);
    assert_eq!(extended.usage.cache_write, 3);

    // First message differs after "user:hello w" (12 chars): "user:hello wide" + 2 + 9 = 26.
    let edited = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hello wide", 1), user_msg("next", 1)],
            tools: None,
        },
        opts(),
    )
    .await;
    assert_eq!(edited.usage.input, 0);
    assert_eq!(edited.usage.cache_read, 3);
    assert_eq!(edited.usage.cache_write, 4);
}

#[tokio::test]
#[serial]
async fn does_not_simulate_caching_when_cache_retention_is_none() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("second".to_owned()),
            None,
        )),
    ]);
    let model = reg.get_model();
    regs.push(reg);

    let mut context = Context {
        system_prompt: None,
        messages: vec![user_msg("hello", time::now_ms())],
        tools: None,
    };

    complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    context.messages.push(Message::Assistant(faux_assistant_message(
        FauxAssistantMessageContent::Text("first".to_owned()),
        None,
    )));
    context.messages.push(user_msg("follow up", time::now_ms() + 1));

    let second = complete(
        &model,
        &context,
        Some(
            StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::None),
                ..Default::default()
            }
            .into(),
        ),
    )
    .await;
    assert_eq!(second.usage.cache_read, 0);
    assert_eq!(second.usage.cache_write, 0);
}

#[tokio::test]
#[serial]
async fn streams_thinking_text_and_partial_tool_call_deltas() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Thinking(faux_thinking("thinking text")),
            FauxContentBlock::Text(faux_text("answer text")),
            FauxContentBlock::ToolCall(faux_tool_call(
                "echo",
                json_obj(json!({ "text": "hi", "count": 12 })),
                Some(FauxToolCallOptions {
                    id: Some("tool-1".to_owned()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let mut events = Vec::new();
    let mut tool_call_deltas = Vec::new();
    let mut s = stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    );
    while let Some(event) = s.next().await {
        events.push(event_type_name(&event));
        if let AssistantMessageEvent::ToolCallDelta { delta, .. } = event {
            tool_call_deltas.push(delta);
        }
    }

    assert!(events.contains(&"thinking_start"));
    assert!(events.contains(&"thinking_delta"));
    assert!(events.contains(&"text_start"));
    assert!(events.contains(&"text_delta"));
    assert!(events.contains(&"toolcall_start"));
    assert!(events.contains(&"toolcall_delta"));
    assert!(events.contains(&"toolcall_end"));
    assert!(tool_call_deltas.len() > 1);
    let parsed: Value = serde_json::from_str(&tool_call_deltas.concat()).unwrap();
    assert_eq!(parsed, json!({ "text": "hi", "count": 12 }));
}

#[tokio::test]
#[serial]
async fn streams_an_exact_event_order_for_fixed_size_chunks() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Thinking(faux_thinking("go")),
            FauxContentBlock::Text(faux_text("ok")),
            FauxContentBlock::ToolCall(faux_tool_call(
                "echo",
                JsonObject::new(),
                Some(FauxToolCallOptions {
                    id: Some("tool-1".to_owned()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    match &events[0] {
        AssistantMessageEvent::Start { partial } => assert_eq!(partial.stop_reason, StopReason::Pending),
        other => panic!("expected start event, got {other:?}"),
    }
    let names: Vec<&str> = events.iter().map(event_type_name).collect();
    assert_eq!(
        names,
        vec![
            "start",
            "thinking_start",
            "thinking_delta",
            "thinking_end",
            "text_start",
            "text_delta",
            "text_end",
            "toolcall_start",
            "toolcall_delta",
            "toolcall_end",
            "done",
        ]
    );
}

#[tokio::test]
#[serial]
async fn streams_multiple_tool_calls_in_one_message() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::ToolCall(faux_tool_call(
                "echo",
                json_obj(json!({ "text": "one" })),
                Some(FauxToolCallOptions {
                    id: Some("tool-1".to_owned()),
                }),
            )),
            FauxContentBlock::ToolCall(faux_tool_call(
                "echo",
                json_obj(json!({ "text": "two" })),
                Some(FauxToolCallOptions {
                    id: Some("tool-2".to_owned()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallStart { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallEnd { .. }))
            .count(),
        2
    );
}

#[tokio::test]
#[serial]
async fn streams_an_explicit_assistant_error_message_as_a_terminal_error() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    }));
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text("partial".to_owned()), None);
    msg.stop_reason = StopReason::Error;
    msg.error_message = Some("upstream failed".to_owned());
    reg.set_responses(vec![FauxResponseStep::Message(msg)]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    let names: Vec<&str> = events.iter().map(event_type_name).collect();
    assert_eq!(names, vec!["start", "text_start", "text_delta", "text_end", "error"]);
    match events.last() {
        Some(AssistantMessageEvent::Error { reason, error }) => {
            assert_eq!(*reason, StopReason::Error);
            assert_eq!(error.stop_reason, StopReason::Error);
            assert_eq!(error.error_message.as_deref(), Some("upstream failed"));
        }
        other => panic!("expected terminal error event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn streams_an_explicit_assistant_aborted_message_as_a_terminal_error() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    }));
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text("partial".to_owned()), None);
    msg.stop_reason = StopReason::Aborted;
    msg.error_message = Some("Request was aborted".to_owned());
    reg.set_responses(vec![FauxResponseStep::Message(msg)]);
    let model = reg.get_model();
    regs.push(reg);

    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    ))
    .await;

    let names: Vec<&str> = events.iter().map(event_type_name).collect();
    assert_eq!(names, vec!["start", "text_start", "text_delta", "text_end", "error"]);
    match events.last() {
        Some(AssistantMessageEvent::Error { reason, error }) => {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
            assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
        }
        other => panic!("expected terminal error event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn supports_aborting_before_the_first_chunk() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(50.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    }));
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("abcdefghijklmnopqrstuvwxyz".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let controller = AbortController::new();
    controller.abort(None);
    let events = collect_events(stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        Some(
            StreamOptions {
                signal: Some(controller.signal()),
                ..Default::default()
            }
            .into(),
        ),
    ))
    .await;

    assert_eq!(events.len(), 1);
    match &events[0] {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
        }
        other => panic!("expected error event, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn supports_aborting_mid_text_stream_when_paced() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    }));
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("abcdefghijklmnopqrstuvwxyz".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    regs.push(reg);

    let controller = AbortController::new();
    let mut events = Vec::new();
    let mut text_delta_count = 0;
    let mut s = stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        Some(
            StreamOptions {
                signal: Some(controller.signal()),
                ..Default::default()
            }
            .into(),
        ),
    );
    while let Some(event) = s.next().await {
        let name = event_type_name(&event);
        events.push(name);
        if name == "text_delta" {
            text_delta_count += 1;
            controller.abort(None);
        }
    }

    assert_eq!(text_delta_count, 1);
    assert!(events.contains(&"text_start"));
    assert!(events.contains(&"text_delta"));
    assert!(events.contains(&"error"));
    assert!(!events.contains(&"text_end"));
}

#[tokio::test]
#[serial]
async fn supports_aborting_mid_thinking_stream_when_paced() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    }));
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Text("ignored".to_owned()), None);
    msg.content = vec![AssistantContentBlock::Thinking(ThinkingContent::new(
        "abcdefghijklmnopqrstuvwxyz",
    ))];
    reg.set_responses(vec![FauxResponseStep::Message(msg)]);
    let model = reg.get_model();
    regs.push(reg);

    let controller = AbortController::new();
    let mut events = Vec::new();
    let mut thinking_delta_count = 0;
    let mut s = stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        Some(
            StreamOptions {
                signal: Some(controller.signal()),
                ..Default::default()
            }
            .into(),
        ),
    );
    while let Some(event) = s.next().await {
        let name = event_type_name(&event);
        events.push(name);
        if name == "thinking_delta" {
            thinking_delta_count += 1;
            controller.abort(None);
        }
    }

    assert_eq!(thinking_delta_count, 1);
    assert!(events.contains(&"thinking_start"));
    assert!(events.contains(&"thinking_delta"));
    assert!(events.contains(&"error"));
    assert!(!events.contains(&"thinking_end"));
}

#[tokio::test]
#[serial]
async fn supports_aborting_mid_toolcall_stream_when_paced() {
    let mut regs = AutoUnregister::new();
    let reg = register_faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(3),
            max: Some(3),
        }),
        ..Default::default()
    }));
    let msg: AssistantMessage = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "echo",
            json_obj(json!({ "text": "abcdefghijklmnopqrstuvwxyz", "count": 123456789 })),
            Some(FauxToolCallOptions {
                id: Some("tool-1".to_owned()),
            }),
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    );
    reg.set_responses(vec![FauxResponseStep::Message(msg)]);
    let model = reg.get_model();
    regs.push(reg);

    let controller = AbortController::new();
    let mut events = Vec::new();
    let mut toolcall_delta_count = 0;
    let mut s = stream(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        Some(
            StreamOptions {
                signal: Some(controller.signal()),
                ..Default::default()
            }
            .into(),
        ),
    );
    while let Some(event) = s.next().await {
        let name = event_type_name(&event);
        events.push(name);
        if name == "toolcall_delta" {
            toolcall_delta_count += 1;
            controller.abort(None);
        }
    }

    assert_eq!(toolcall_delta_count, 1);
    assert!(events.contains(&"toolcall_start"));
    assert!(events.contains(&"toolcall_delta"));
    assert!(events.contains(&"error"));
    assert!(!events.contains(&"toolcall_end"));
}

#[tokio::test]
#[serial]
async fn unregisters_the_provider() {
    let reg = register_faux_provider(None);
    reg.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_owned()),
        None,
    ))]);
    let model = reg.get_model();
    let api = reg.api().to_owned();
    reg.unregister();

    let err_msg = complete(
        &model,
        &Context {
            system_prompt: None,
            messages: vec![user_msg("hi", time::now_ms())],
            tools: None,
        },
        None,
    )
    .await;
    assert_eq!(err_msg.stop_reason, StopReason::Error);
    assert!(
        err_msg
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains(&format!("No API provider registered for api: {api}")),
        "unexpected error: {:?}",
        err_msg.error_message
    );
}

//! Port of packages/agent/test/agent-loop.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_agent_core::{
    AfterToolCallResult, AgentContext, AgentEvent, AgentEventStream, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentMessage, AgentTool, AgentToolCall, AgentToolResult, BeforeToolCallResult, FinishTurnAction,
    FinishTurnDecision, PrepareRequestResult, RunToolCallOptions, StreamFn, ThinkingLevel, ToolExecutionMode,
    agent_loop, agent_loop_continue, run_agent_loop, run_tool_call, set_default_stream_fn,
};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, FauxAssistantMessageContent, FauxToolCallOptions,
    JsonObject, Message, Model, StopReason, SystemMessage, SystemMessageContent, UserContentBlock, UserMessage,
    UserMessageContent, create_assistant_message_event_stream, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_js::Error;
use pi_js::abort::AbortController;
use pi_js::time::now_ms;
use pi_schema::{Schema, arr, obj, str};
use serde_json::{Value, json};
use serial_test::serial;

struct DefaultStreamReset;

impl Drop for DefaultStreamReset {
    fn drop(&mut self) {
        set_default_stream_fn(None);
    }
}

fn create_model() -> Model {
    serde_json::from_value(json!({
        "id": "mock",
        "name": "mock",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": "https://example.invalid",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 8192,
        "maxTokens": 2048
    }))
    .expect("valid test model")
}

fn json_obj(val: Value) -> JsonObject {
    serde_json::from_value(val).expect("valid JsonObject")
}

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::from(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: now_ms(),
    })
}

fn assistant_msg(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AssistantMessage {
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Blocks(Vec::new()), None);
    msg.content = content;
    msg.api = "openai-responses".to_string();
    msg.provider = "openai".to_string();
    msg.model = "mock".to_string();
    msg.stop_reason = stop_reason;
    msg.timestamp = now_ms();
    msg
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(faux_text(text))
}

fn tc_block(id: &str, name: &str, args: Value) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(faux_tool_call(
        name,
        json_obj(args),
        Some(FauxToolCallOptions {
            id: Some(id.to_string()),
        }),
    ))
}

fn make_tool_call(id: &str, name: &str, args: Value) -> AgentToolCall {
    AgentToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: json_obj(args),
    }
}

fn identity_converter() -> pi_agent_core::ConvertToLlmFn {
    Arc::new(|messages| {
        Box::pin(async move { Ok(messages.into_iter().filter_map(AgentMessage::into_message).collect()) })
    })
}

fn base_config() -> AgentLoopConfig {
    AgentLoopConfig::new(create_model(), identity_converter())
}

fn spawn_done_stream(msg: AssistantMessage, reason: StopReason) -> pi_ai::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let stream_task = stream.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        stream_task.push(AssistantMessageEvent::Done {
            reason,
            message: msg.clone(),
        });
        stream_task.end(Some(msg));
    });
    stream
}

fn spawn_error_stream(err: AssistantMessage, reason: StopReason) -> pi_ai::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let stream_task = stream.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        stream_task.push(AssistantMessageEvent::Error {
            reason,
            error: err.clone(),
        });
        stream_task.end(Some(err));
    });
    stream
}

async fn collect_stream(mut stream: AgentEventStream) -> (Vec<AgentEvent>, Vec<AgentMessage>) {
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    let result = stream.result().await.unwrap_or_default();
    (events, result)
}

fn make_echo_tool(execution_mode: Option<ToolExecutionMode>, executed: Arc<Mutex<Vec<Value>>>) -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _signal, _on_update| {
            let executed = Arc::clone(&executed);
            Box::pin(async move {
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                executed.lock().unwrap().push(value.clone());
                let text = match &value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text(&format!("echoed: {text}")))],
                    details: Some(json!({ "value": value })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                    control: None,
                })
            })
        }),
        replay: None,
    }
}

fn make_noop_tool() -> AgentTool {
    AgentTool {
        name: "noop".to_string(),
        label: "Noop".to_string(),
        description: "Noop tool".to_string(),
        parameters: obj([] as [(&str, Schema); 0]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _signal, _on_update| Box::pin(async { Ok(AgentToolResult::text("done")) })),
        replay: None,
    }
}

fn tool_result_texts(messages: &[AgentMessage]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr)) => {
                let text = tr
                    .content
                    .iter()
                    .find_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((tr.tool_call_id.clone(), text))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[serial]
async fn uses_default_stream_fn_when_omitted() {
    let _guard = DefaultStreamReset;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let default_fn: StreamFn = Arc::new(move |_model, _ctx, _opts| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("fallback")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });
    set_default_stream_fn(Some(default_fn));

    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let stream = agent_loop(vec![user_msg("Hello")], context, base_config(), None, None);
    let (_events, messages) = collect_stream(stream).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(messages.len(), 2);
}

#[tokio::test]
async fn emits_events_with_agent_message_types() {
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(Vec::new()),
    };
    let stream_fn: StreamFn = Arc::new(|_model, _ctx, _opts| {
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("Hi there!")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(vec![user_msg("Hello")], context, base_config(), None, Some(stream_fn));
    let (events, messages) = collect_stream(stream).await;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), Some("user"));
    assert_eq!(messages[1].role(), Some("assistant"));

    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentStart)));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnStart)));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::MessageStart { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::MessageEnd { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::TurnEnd { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentEnd { .. })));
}

#[tokio::test]
async fn builds_provider_context_exclusively_from_transcript_messages() {
    let initial_system = SystemMessage {
        content: SystemMessageContent::Text("Transcript prompt".to_string()),
        sections: None,
        tools_added: Some(Vec::new()),
        tools_removed: None,
        timestamp: 1,
    };
    let expected_system = initial_system.clone();
    let seen_clean = Arc::new(Mutex::new(false));
    let seen_clean_ref = Arc::clone(&seen_clean);

    let stream_fn: StreamFn = Arc::new(move |_model, provider_ctx, _opts| {
        let seen_clean_ref = Arc::clone(&seen_clean_ref);
        let expected_system = expected_system.clone();
        Box::pin(async move {
            assert!(provider_ctx.system_prompt.is_none());
            assert!(provider_ctx.tools.is_none());
            assert_eq!(provider_ctx.messages.first(), Some(&Message::System(expected_system)));
            *seen_clean_ref.lock().unwrap() = true;
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("done")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(
        vec![AgentMessage::from(initial_system), user_msg("Hello")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;
    assert!(*seen_clean.lock().unwrap());
}

#[tokio::test]
async fn handles_custom_message_types_via_convert_to_llm() {
    let notification = AgentMessage::custom(json!({
        "role": "notification",
        "text": "This is a notification",
        "timestamp": now_ms()
    }));
    let converted = Arc::new(Mutex::new(Vec::<Message>::new()));
    let converted_ref = Arc::clone(&converted);

    let mut cfg = base_config();
    cfg.convert_to_llm = Arc::new(move |msgs| {
        let converted_ref = Arc::clone(&converted_ref);
        Box::pin(async move {
            let filtered: Vec<Message> = msgs
                .into_iter()
                .filter(|m| m.role() != Some("notification"))
                .filter_map(AgentMessage::into_message)
                .collect();
            *converted_ref.lock().unwrap() = filtered.clone();
            Ok(filtered)
        })
    });

    let stream_fn: StreamFn = Arc::new(|_m, _ctx, _opts| {
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("Response")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(
        vec![user_msg("Hello")],
        AgentContext {
            messages: vec![notification],
            tools: Some(Vec::new()),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    let guard = converted.lock().unwrap();
    assert_eq!(guard.len(), 1);
    assert!(matches!(guard[0], Message::User(_)));
}

#[tokio::test]
async fn applies_transform_context_before_convert_to_llm() {
    let transformed = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let converted = Arc::new(Mutex::new(Vec::<Message>::new()));
    let transformed_ref = Arc::clone(&transformed);
    let converted_ref = Arc::clone(&converted);

    let mut cfg = base_config();
    cfg.transform_context = Some(Arc::new(move |msgs, _signal| {
        let transformed_ref = Arc::clone(&transformed_ref);
        Box::pin(async move {
            let len = msgs.len();
            let kept = if len > 2 { msgs[len - 2..].to_vec() } else { msgs };
            *transformed_ref.lock().unwrap() = kept.clone();
            Ok(kept)
        })
    }));
    cfg.convert_to_llm = Arc::new(move |msgs| {
        let converted_ref = Arc::clone(&converted_ref);
        Box::pin(async move {
            let out: Vec<Message> = msgs.into_iter().filter_map(AgentMessage::into_message).collect();
            *converted_ref.lock().unwrap() = out.clone();
            Ok(out)
        })
    });

    let context = AgentContext {
        messages: vec![
            user_msg("old 1"),
            AgentMessage::from(assistant_msg(vec![text_block("resp 1")], StopReason::Stop)),
            user_msg("old 2"),
            AgentMessage::from(assistant_msg(vec![text_block("resp 2")], StopReason::Stop)),
        ],
        tools: Some(Vec::new()),
    };

    let stream_fn: StreamFn = Arc::new(|_m, _ctx, _opts| {
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("Final")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(vec![user_msg("new")], context, cfg, None, Some(stream_fn));
    let _ = collect_stream(stream).await;

    assert_eq!(transformed.lock().unwrap().len(), 2);
    assert_eq!(converted.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn handles_tool_calls_and_results() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("all done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;

    assert_eq!(*executed.lock().unwrap(), vec![json!("hello")]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
    );
    let tool_end = events.iter().find_map(|e| match e {
        AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
        _ => None,
    });
    assert_eq!(tool_end, Some(false));
    assert_eq!(
        messages.iter().filter_map(AgentMessage::role).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "toolResult", "assistant"]
    );
}

// #10549
#[tokio::test]
async fn records_how_long_execute_took_on_tool_result_excluding_hooks() {
    let tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _signal, _on_update| {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                let text = value.as_str().unwrap_or("").to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text(&text))],
                    details: Some(json!({ "value": value })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let mut cfg = base_config();
    cfg.before_tool_call = Some(Arc::new(|ctx, _sig| {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            if ctx.tool_call.id == "blocked" {
                Ok(Some(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("no".to_string()),
                    ..Default::default()
                }))
            } else {
                Ok(None)
            }
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("ran", "echo", json!({ "value": "a" })),
                            tc_block("blocked", "echo", json!({ "value": "b" })),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("go")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;

    let results: Vec<&pi_ai::ToolResultMessage> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);

    let ran = results[0];
    let blocked = results[1];
    let ran_duration = ran.duration_ms.expect("ran tool result should record duration_ms");
    assert!(ran_duration >= 25, "expected duration_ms >= 25, got {ran_duration}");
    assert!(ran_duration < 120, "expected duration_ms < 120, got {ran_duration}");

    assert!(blocked.is_error);
    assert_eq!(blocked.duration_ms, None);
    let blocked_json = pi_js::json::parse(&pi_js::json::stringify(blocked)).expect("serialize blocked tool result");
    assert!(blocked_json.get("durationMs").is_none());

    let mut end_durations = Vec::new();
    for ev in &events {
        if let AgentEvent::ToolExecutionEnd {
            tool_call_id,
            duration_ms,
            ..
        } = ev
        {
            end_durations.push((tool_call_id.clone(), *duration_ms));
            if tool_call_id == "blocked" {
                let ev_json = pi_js::json::parse(&pi_js::json::stringify(ev)).expect("serialize tool_execution_end");
                assert!(ev_json.get("durationMs").is_none());
            }
        }
    }
    assert!(end_durations.contains(&("ran".to_string(), Some(ran_duration))));
    assert!(end_durations.contains(&("blocked".to_string(), None)));
}

#[tokio::test]
async fn does_not_execute_tool_calls_from_length_truncated_assistant_message() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);

    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hel" }))],
                        StopReason::Length,
                    ),
                    StopReason::Length,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;

    assert!(executed.lock().unwrap().is_empty());
    let (is_err, text) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd { is_error, result, .. } => {
                let t = result
                    .content
                    .iter()
                    .find_map(|b| match b {
                        UserContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((*is_error, t))
            }
            _ => None,
        })
        .expect("expected tool_execution_end");
    assert!(is_err);
    assert!(text.contains("output token limit"), "unexpected message: {text}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(messages.last().and_then(AgentMessage::role), Some("assistant"));
}

#[tokio::test]
async fn executes_mutated_before_tool_call_args_without_revalidation() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));
    let mut cfg = base_config();
    cfg.before_tool_call = Some(Arc::new(|_ctx, _sig| {
        Box::pin(async move {
            Ok(Some(BeforeToolCallResult {
                args: Some(json!({ "value": 123 })),
                ..Default::default()
            }))
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;
    assert_eq!(*executed.lock().unwrap(), vec![json!(123)]);
}

#[tokio::test]
async fn prepares_tool_arguments_for_validation() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let executed_ref = Arc::clone(&executed);
    let replace_schema = obj([("oldText", str()), ("newText", str())]);
    let tool = AgentTool {
        name: "edit".to_string(),
        label: "Edit".to_string(),
        description: "Edit tool".to_string(),
        parameters: obj([("edits", arr(replace_schema))]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: Some(Arc::new(|args| {
            let Some(obj) = args.as_object() else {
                return args;
            };
            if let (Some(Value::String(old)), Some(Value::String(new))) = (obj.get("oldText"), obj.get("newText")) {
                let mut edits = obj.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
                edits.push(json!({ "oldText": old, "newText": new }));
                return json!({ "edits": edits });
            }
            args
        })),
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let executed_ref = Arc::clone(&executed_ref);
            Box::pin(async move {
                let edits = params.get("edits").cloned().unwrap_or(Value::Null);
                executed_ref.lock().unwrap().push(edits.clone());
                Ok(AgentToolResult::text("edited"))
            })
        }),
        replay: None,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block(
                            "tool-1",
                            "edit",
                            json!({ "oldText": "before", "newText": "after" }),
                        )],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("edit something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(
        *executed.lock().unwrap(),
        vec![json!([{ "oldText": "before", "newText": "after" }])]
    );
}

#[tokio::test]
async fn emits_tool_execution_end_in_completion_order_but_persists_tool_results_in_source_order() {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let release_first = Arc::new(Mutex::new(Some(tx)));
    let rx_slot = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let first_resolved = Arc::new(Mutex::new(false));
    let parallel_observed = Arc::new(Mutex::new(false));

    let rx_ref = Arc::clone(&rx_slot);
    let first_resolved_ref = Arc::clone(&first_resolved);
    let parallel_observed_ref = Arc::clone(&parallel_observed);

    let tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let rx_ref = Arc::clone(&rx_ref);
            let first_resolved_ref = Arc::clone(&first_resolved_ref);
            let parallel_observed_ref = Arc::clone(&parallel_observed_ref);
            Box::pin(async move {
                let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                if value == "first" {
                    let rx_opt = rx_ref.lock().await.take();
                    if let Some(rx) = rx_opt {
                        let _ = rx.await;
                    }
                    *first_resolved_ref.lock().unwrap() = true;
                }
                if value == "second" && !*first_resolved_ref.lock().unwrap() {
                    *parallel_observed_ref.lock().unwrap() = true;
                }
                Ok(AgentToolResult::text(format!("echoed: {value}")))
            })
        }),
        replay: None,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let release_ref = Arc::clone(&release_first);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        let release_ref = Arc::clone(&release_ref);
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let stream_task = stream.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                if idx == 0 {
                    let msg = assistant_msg(
                        vec![
                            tc_block("tool-1", "echo", json!({ "value": "first" })),
                            tc_block("tool-2", "echo", json!({ "value": "second" })),
                        ],
                        StopReason::ToolUse,
                    );
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if let Some(tx) = release_ref.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                } else {
                    let msg = assistant_msg(vec![text_block("done")], StopReason::Stop);
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                }
            });
            Ok(stream)
        })
    });

    let mut cfg = base_config();
    cfg.tool_execution = Some(ToolExecutionMode::Parallel);
    let stream = agent_loop(
        vec![user_msg("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let (events, _messages) = collect_stream(stream).await;

    let end_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let result_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd {
                message: AgentMessage::Message(Message::ToolResult(tr)),
            } => Some(tr.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let turn_ids: Vec<String> = events
        .iter()
        .flat_map(|e| match e {
            AgentEvent::TurnEnd { tool_results, .. } => tool_results.iter().map(|r| r.tool_call_id.clone()).collect(),
            _ => Vec::new(),
        })
        .collect();

    assert!(*parallel_observed.lock().unwrap());
    assert_eq!(end_ids, vec!["tool-2", "tool-1"]);
    assert_eq!(result_ids, vec!["tool-1", "tool-2"]);
    assert_eq!(turn_ids, vec!["tool-1", "tool-2"]);
}

#[tokio::test]
async fn injects_queued_messages_after_all_tool_calls_complete() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));
    let delivered = Arc::new(Mutex::new(false));
    let saw_interrupt = Arc::new(Mutex::new(false));

    let executed_for_steering = Arc::clone(&executed);
    let delivered_ref = Arc::clone(&delivered);
    let mut cfg = base_config();
    cfg.tool_execution = Some(ToolExecutionMode::Sequential);
    cfg.get_steering_messages = Some(Arc::new(move || {
        let executed_for_steering = Arc::clone(&executed_for_steering);
        let delivered_ref = Arc::clone(&delivered_ref);
        Box::pin(async move {
            let mut flag = delivered_ref.lock().unwrap();
            if !executed_for_steering.lock().unwrap().is_empty() && !*flag {
                *flag = true;
                return Ok(vec![user_msg("interrupt")]);
            }
            Ok(Vec::new())
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let saw_interrupt_ref = Arc::clone(&saw_interrupt);
    let stream_fn: StreamFn = Arc::new(move |_m, ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        let saw_interrupt_ref = Arc::clone(&saw_interrupt_ref);
        Box::pin(async move {
            if idx == 1 {
                *saw_interrupt_ref.lock().unwrap() = ctx.messages.iter().any(|m| {
                    matches!(m, Message::User(UserMessage { content: UserMessageContent::Text(s), .. }) if s == "interrupt")
                });
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("tool-1", "echo", json!({ "value": "first" })),
                            tc_block("tool-2", "echo", json!({ "value": "second" })),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("start")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let (events, _messages) = collect_stream(stream).await;

    assert_eq!(*executed.lock().unwrap(), vec![json!("first"), json!("second")]);
    let seq: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart { message } => match message {
                AgentMessage::Message(Message::ToolResult(tr)) => Some(format!("tool:{}", tr.tool_call_id)),
                AgentMessage::Message(Message::User(UserMessage {
                    content: UserMessageContent::Text(t),
                    ..
                })) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let p1 = seq.iter().position(|s| s == "tool:tool-1").unwrap();
    let p2 = seq.iter().position(|s| s == "tool:tool-2").unwrap();
    let pi = seq.iter().position(|s| s == "interrupt").unwrap();
    assert!(p1 < pi && p2 < pi);
    assert!(*saw_interrupt.lock().unwrap());
}

#[tokio::test]
async fn answers_every_tool_call_of_sequential_batch_aborted_mid_batch() {
    let controller = Arc::new(AbortController::new());
    let executed = Arc::new(Mutex::new(Vec::<String>::new()));
    let executed_ref = Arc::clone(&executed);
    let ctrl_ref = Arc::clone(&controller);

    let tool = AgentTool {
        name: "slow".to_string(),
        label: "Slow".to_string(),
        description: "Slow tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let executed_ref = Arc::clone(&executed_ref);
            let ctrl_ref = ctrl_ref.clone();
            Box::pin(async move {
                let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                executed_ref.lock().unwrap().push(value);
                ctrl_ref.abort(None);
                Err(Error::msg("interrupted"))
            })
        }),
        replay: None,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("tool-1", "slow", json!({ "value": "first" })),
                            tc_block("tool-2", "slow", json!({ "value": "second" })),
                            tc_block("tool-3", "slow", json!({ "value": "third" })),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        Some(controller.signal()),
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;

    assert_eq!(*executed.lock().unwrap(), vec!["first"]);
    assert_eq!(
        tool_result_texts(&messages),
        vec![
            ("tool-1".to_string(), "interrupted".to_string()),
            ("tool-2".to_string(), "Operation aborted".to_string()),
            ("tool-3".to_string(), "Operation aborted".to_string()),
        ]
    );
    let starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let ends: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec!["tool-1", "tool-2", "tool-3"]);
    assert_eq!(ends, vec!["tool-1", "tool-2", "tool-3"]);
}

#[tokio::test]
async fn answers_every_tool_call_of_parallel_batch_aborted_while_calls_are_prepared() {
    let controller = Arc::new(AbortController::new());
    let executed = Arc::new(Mutex::new(Vec::<String>::new()));
    let hook_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let executed_ref = Arc::clone(&executed);
    let hook_calls_ref = Arc::clone(&hook_calls);
    let ctrl_ref = Arc::clone(&controller);

    let tool = AgentTool {
        name: "slow".to_string(),
        label: "Slow".to_string(),
        description: "Slow tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let executed_ref = Arc::clone(&executed_ref);
            Box::pin(async move {
                let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                executed_ref.lock().unwrap().push(value.clone());
                Ok(AgentToolResult::text(value))
            })
        }),
        replay: None,
    };

    let mut cfg = base_config();
    cfg.before_tool_call = Some(Arc::new(move |ctx, _sig| {
        let hook_calls_ref = Arc::clone(&hook_calls_ref);
        let ctrl_ref = ctrl_ref.clone();
        Box::pin(async move {
            hook_calls_ref.lock().unwrap().push(ctx.tool_call.id.clone());
            if ctx.tool_call.id == "tool-2" {
                ctrl_ref.abort(None);
            }
            Ok(None)
        })
    }));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("tool-1", "slow", json!({ "value": "first" })),
                            tc_block("tool-2", "slow", json!({ "value": "second" })),
                            tc_block("tool-3", "slow", json!({ "value": "third" })),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        Some(controller.signal()),
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;

    assert!(executed.lock().unwrap().is_empty());
    assert_eq!(*hook_calls.lock().unwrap(), vec!["tool-1", "tool-2"]);
    assert_eq!(
        tool_result_texts(&messages),
        vec![
            ("tool-1".to_string(), "Operation aborted".to_string()),
            ("tool-2".to_string(), "Operation aborted".to_string()),
            ("tool-3".to_string(), "Operation aborted".to_string()),
        ]
    );
    let starts: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let mut ends: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    ends.sort();
    assert_eq!(starts, vec!["tool-1", "tool-2", "tool-3"]);
    assert_eq!(ends, vec!["tool-1", "tool-2", "tool-3"]);
}

#[tokio::test]
async fn forces_sequential_execution_when_one_tool_has_execution_mode_sequential() {
    let execution_order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let release_slow = Arc::new(Mutex::new(Some(tx)));
    let rx_slot = Arc::new(tokio::sync::Mutex::new(Some(rx)));

    let order_slow = Arc::clone(&execution_order);
    let rx_ref = Arc::clone(&rx_slot);
    let slow_tool = AgentTool {
        name: "slow".to_string(),
        label: "Slow".to_string(),
        description: "Slow tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let order_slow = Arc::clone(&order_slow);
            let rx_ref = Arc::clone(&rx_ref);
            Box::pin(async move {
                let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                order_slow.lock().unwrap().push(format!("slow:{value}"));
                if value == "a"
                    && let Some(rx) = rx_ref.lock().await.take()
                {
                    let _ = rx.await;
                }
                Ok(AgentToolResult::text(format!("slow: {value}")))
            })
        }),
        replay: None,
    };

    let order_fast = Arc::clone(&execution_order);
    let fast_tool = AgentTool {
        name: "fast".to_string(),
        label: "Fast".to_string(),
        description: "Fast tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _on_update| {
            let order_fast = Arc::clone(&order_fast);
            Box::pin(async move {
                let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                order_fast.lock().unwrap().push(format!("fast:{value}"));
                Ok(AgentToolResult::text(format!("fast: {value}")))
            })
        }),
        replay: None,
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let release_ref = Arc::clone(&release_slow);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        let release_ref = Arc::clone(&release_ref);
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let stream_task = stream.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                if idx == 0 {
                    let msg = assistant_msg(
                        vec![
                            tc_block("tool-1", "slow", json!({ "value": "a" })),
                            tc_block("tool-2", "fast", json!({ "value": "b" })),
                        ],
                        StopReason::ToolUse,
                    );
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if let Some(tx) = release_ref.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                } else {
                    let msg = assistant_msg(vec![text_block("done")], StopReason::Stop);
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                }
            });
            Ok(stream)
        })
    });

    let stream = agent_loop(
        vec![user_msg("run both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![slow_tool, fast_tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    let order = execution_order.lock().unwrap();
    assert_eq!(order.as_slice(), &["slow:a", "fast:b"]);
}

#[tokio::test]
async fn allows_parallel_execution_when_all_tools_have_execution_mode_parallel() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mut tool = make_echo_tool(Some(ToolExecutionMode::Parallel), Arc::clone(&executed));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let release_first = Arc::new(Mutex::new(Some(tx)));
    let rx_slot = Arc::new(tokio::sync::Mutex::new(Some(rx)));
    let first_done = Arc::new(Mutex::new(false));
    let parallel_observed = Arc::new(Mutex::new(false));

    let rx_ref = Arc::clone(&rx_slot);
    let first_done_ref = Arc::clone(&first_done);
    let parallel_observed_ref = Arc::clone(&parallel_observed);
    tool.execute = Arc::new(move |_id, params, _sig, _on_update| {
        let rx_ref = Arc::clone(&rx_ref);
        let first_done_ref = Arc::clone(&first_done_ref);
        let parallel_observed_ref = Arc::clone(&parallel_observed_ref);
        Box::pin(async move {
            let value = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
            if value == "first" {
                if let Some(rx) = rx_ref.lock().await.take() {
                    let _ = rx.await;
                }
                *first_done_ref.lock().unwrap() = true;
            }
            if value == "second" && !*first_done_ref.lock().unwrap() {
                *parallel_observed_ref.lock().unwrap() = true;
            }
            Ok(AgentToolResult::text(format!("echoed: {value}")))
        })
    });

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    let release_ref = Arc::clone(&release_first);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = calls_ref.fetch_add(1, Ordering::SeqCst);
        let release_ref = Arc::clone(&release_ref);
        Box::pin(async move {
            let stream = create_assistant_message_event_stream();
            let stream_task = stream.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                if idx == 0 {
                    let msg = assistant_msg(
                        vec![
                            tc_block("tool-1", "echo", json!({ "value": "first" })),
                            tc_block("tool-2", "echo", json!({ "value": "second" })),
                        ],
                        StopReason::ToolUse,
                    );
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::ToolUse,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if let Some(tx) = release_ref.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                } else {
                    let msg = assistant_msg(vec![text_block("done")], StopReason::Stop);
                    stream_task.push(AssistantMessageEvent::Done {
                        reason: StopReason::Stop,
                        message: msg.clone(),
                    });
                    stream_task.end(Some(msg));
                }
            });
            Ok(stream)
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;
    assert!(*parallel_observed.lock().unwrap());
}

#[tokio::test]
async fn runs_finish_turn_after_tool_result_messages_and_before_turn_end() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mut tool = make_echo_tool(None, Arc::clone(&executed));
    tool.execute = Arc::new(|_id, params, _sig, _on_update| {
        Box::pin(async move {
            let val = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
            let mut r = AgentToolResult::text(val);
            r.terminate = Some(true);
            Ok(r)
        })
    });

    let ordering = Arc::new(Mutex::new(Vec::<String>::new()));
    let ordering_finish = Arc::clone(&ordering);
    let mut cfg = base_config();
    cfg.finish_turn = Some(Arc::new(move |ctx, _sig| {
        let ordering_finish = Arc::clone(&ordering_finish);
        Box::pin(async move {
            ordering_finish.lock().unwrap().push("finishTurn".to_string());
            assert_eq!(ctx.tool_results.len(), 1);
            assert_eq!(
                ctx.context.messages.last().and_then(AgentMessage::role),
                Some("toolResult")
            );
            Ok(None)
        })
    }));

    let ordering_emit = Arc::clone(&ordering);
    let emit: pi_agent_core::AgentEventSink = Arc::new(move |event| {
        let mut guard = ordering_emit.lock().unwrap();
        match event {
            AgentEvent::MessageEnd { message } => {
                guard.push(format!("message_end:{}", message.role().unwrap_or("")));
            }
            AgentEvent::TurnEnd { .. } => guard.push("turn_end".to_string()),
            _ => {}
        }
    });

    let stream_fn: StreamFn = Arc::new(|_m, _ctx, _opts| {
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(
                    vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            ))
        })
    });

    let _ = run_agent_loop(
        vec![user_msg("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .unwrap();

    let ord = ordering.lock().unwrap();
    let tail = &ord[ord.len() - 3..];
    assert_eq!(tail, &["message_end:toolResult", "finishTurn", "turn_end"]);
}

#[tokio::test]
async fn runs_finish_turn_for_error_and_aborted_before_turn_end() {
    for reason in [StopReason::Error, StopReason::Aborted] {
        let ordering = Arc::new(Mutex::new(Vec::<String>::new()));
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let steering_polls = Arc::new(AtomicUsize::new(0));
        let follow_up_polls = Arc::new(AtomicUsize::new(0));

        let ordering_fin = Arc::clone(&ordering);
        let sp_ref = Arc::clone(&steering_polls);
        let fp_ref = Arc::clone(&follow_up_polls);
        let pc_ref = Arc::clone(&provider_calls);

        let mut cfg = base_config();
        cfg.finish_turn = Some(Arc::new(move |ctx, _sig| {
            let ordering_fin = Arc::clone(&ordering_fin);
            Box::pin(async move {
                assert_eq!(ctx.message.stop_reason, reason);
                ordering_fin.lock().unwrap().push("finishTurn".to_string());
                Ok(Some(FinishTurnDecision {
                    action: FinishTurnAction::Continue,
                }))
            })
        }));
        cfg.get_steering_messages = Some(Arc::new(move || {
            let sp_ref = Arc::clone(&sp_ref);
            Box::pin(async move {
                sp_ref.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            })
        }));
        cfg.get_follow_up_messages = Some(Arc::new(move || {
            let fp_ref = Arc::clone(&fp_ref);
            Box::pin(async move {
                fp_ref.fetch_add(1, Ordering::SeqCst);
                Ok(vec![user_msg("queued")])
            })
        }));

        let ordering_emit = Arc::clone(&ordering);
        let emit: pi_agent_core::AgentEventSink = Arc::new(move |event| {
            if matches!(event, AgentEvent::TurnEnd { .. }) {
                ordering_emit.lock().unwrap().push("turn_end".to_string());
            }
        });

        let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
            pc_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut err = assistant_msg(Vec::new(), reason);
                err.error_message = Some(format!("{reason:?}"));
                Ok(spawn_error_stream(err, reason))
            })
        });

        let _ = run_agent_loop(
            vec![user_msg("run")],
            AgentContext {
                messages: Vec::new(),
                tools: Some(Vec::new()),
            },
            cfg,
            emit,
            None,
            Some(stream_fn),
        )
        .await
        .unwrap();

        assert_eq!(*ordering.lock().unwrap(), vec!["finishTurn", "turn_end"]);
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
        assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn finish_turn_action_end_skips_queue_polling_and_next_turn_preparation() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let prepare_calls = Arc::new(AtomicUsize::new(0));

    let sp_ref = Arc::clone(&steering_polls);
    let fp_ref = Arc::clone(&follow_up_polls);
    let prep_ref = Arc::clone(&prepare_calls);
    let pc_ref = Arc::clone(&provider_calls);

    let mut cfg = base_config();
    cfg.finish_turn = Some(Arc::new(|_ctx, _sig| {
        Box::pin(async {
            Ok(Some(FinishTurnDecision {
                action: FinishTurnAction::End,
            }))
        })
    }));
    cfg.prepare_next_turn = Some(Arc::new(move |_sig| {
        let prep_ref = Arc::clone(&prep_ref);
        Box::pin(async move {
            prep_ref.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    }));
    cfg.get_steering_messages = Some(Arc::new(move || {
        let sp_ref = Arc::clone(&sp_ref);
        Box::pin(async move {
            sp_ref.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        })
    }));
    cfg.get_follow_up_messages = Some(Arc::new(move || {
        let fp_ref = Arc::clone(&fp_ref);
        Box::pin(async move {
            fp_ref.fetch_add(1, Ordering::SeqCst);
            Ok(vec![user_msg("queued")])
        })
    }));

    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        pc_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![tc_block("tool-1", "noop", json!({}))], StopReason::ToolUse),
                StopReason::ToolUse,
            ))
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![make_noop_tool()]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn finish_turn_continue_makes_one_context_only_request_when_no_natural_request() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let pc_ref = Arc::clone(&provider_calls);
    let fc_ref = Arc::clone(&finish_calls);

    let mut cfg = base_config();
    cfg.finish_turn = Some(Arc::new(move |_ctx, _sig| {
        let count = fc_ref.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(if count == 1 {
                Some(FinishTurnDecision {
                    action: FinishTurnAction::Continue,
                })
            } else {
                None
            })
        })
    }));

    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let count = pc_ref.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block(&format!("response {count}"))], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn finish_turn_continue_lets_natural_tool_result_request_satisfy_continuation() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let pc_ref = Arc::clone(&provider_calls);
    let fc_ref = Arc::clone(&finish_calls);

    let mut cfg = base_config();
    cfg.finish_turn = Some(Arc::new(move |_ctx, _sig| {
        let count = fc_ref.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            Ok(if count == 1 {
                Some(FinishTurnDecision {
                    action: FinishTurnAction::Continue,
                })
            } else {
                None
            })
        })
    }));

    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let count = pc_ref.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if count == 1 {
                Ok(spawn_done_stream(
                    assistant_msg(vec![tc_block("tool-1", "noop", json!({}))], StopReason::ToolUse),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![make_noop_tool()]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn finish_turn_continue_lets_natural_steering_and_follow_up_satisfy_continuation() {
    for kind in ["steering", "follow-up"] {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let finish_calls = Arc::new(AtomicUsize::new(0));
        let steering_polls = Arc::new(AtomicUsize::new(0));
        let follow_up_delivered = Arc::new(Mutex::new(false));
        let second_users = Arc::new(Mutex::new(Vec::<String>::new()));

        let pc_ref = Arc::clone(&provider_calls);
        let fc_ref = Arc::clone(&finish_calls);
        let sp_ref = Arc::clone(&steering_polls);
        let fu_ref = Arc::clone(&follow_up_delivered);
        let su_ref = Arc::clone(&second_users);

        let mut cfg = base_config();
        cfg.finish_turn = Some(Arc::new(move |_ctx, _sig| {
            let count = fc_ref.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                Ok(if count == 1 {
                    Some(FinishTurnDecision {
                        action: FinishTurnAction::Continue,
                    })
                } else {
                    None
                })
            })
        }));
        cfg.get_steering_messages = Some(Arc::new(move || {
            let poll = sp_ref.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if kind == "steering" && poll == 2 {
                    Ok(vec![user_msg(kind)])
                } else {
                    Ok(Vec::new())
                }
            })
        }));
        cfg.get_follow_up_messages = Some(Arc::new(move || {
            let fu_ref = Arc::clone(&fu_ref);
            Box::pin(async move {
                if kind != "follow-up" {
                    return Ok(Vec::new());
                }
                let mut sent = fu_ref.lock().unwrap();
                if *sent {
                    Ok(Vec::new())
                } else {
                    *sent = true;
                    Ok(vec![user_msg(kind)])
                }
            })
        }));

        let stream_fn: StreamFn = Arc::new(move |_m, ctx, _opts| {
            let count = pc_ref.fetch_add(1, Ordering::SeqCst) + 1;
            let su_ref = Arc::clone(&su_ref);
            Box::pin(async move {
                if count == 2 {
                    let texts: Vec<String> = ctx
                        .messages
                        .iter()
                        .filter_map(|m| match m {
                            Message::User(UserMessage {
                                content: UserMessageContent::Text(t),
                                ..
                            }) => Some(t.clone()),
                            _ => None,
                        })
                        .collect();
                    su_ref.lock().unwrap().extend(texts);
                }
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        });

        let stream = agent_loop(
            vec![user_msg("run")],
            AgentContext {
                messages: Vec::new(),
                tools: Some(Vec::new()),
            },
            cfg,
            None,
            Some(stream_fn),
        );
        let _ = collect_stream(stream).await;

        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
        assert!(second_users.lock().unwrap().contains(&kind.to_string()));
    }
}

#[tokio::test]
async fn prepare_request_runs_after_pending_messages_and_replaces_state() {
    let mut replacement = create_model();
    replacement.id = "replacement".to_string();
    replacement.name = "replacement".to_string();
    let expected_model = replacement.clone();

    let canonical = user_msg("canonical projection");
    let canonical_for_req = canonical.clone();
    let steering = user_msg("steering");
    let steering_for_queue = steering.clone();
    let steering_for_req = steering.clone();

    let completed = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let completed_for_emit = Arc::clone(&completed);
    let completed_for_req = Arc::clone(&completed);

    let steering_delivered = Arc::new(Mutex::new(false));
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let prepare_calls_ref = Arc::clone(&prepare_calls);

    let mut cfg = base_config();
    cfg.get_steering_messages = Some(Arc::new(move || {
        let steering_delivered = Arc::clone(&steering_delivered);
        let steering_for_queue = steering_for_queue.clone();
        Box::pin(async move {
            let mut flag = steering_delivered.lock().unwrap();
            if *flag {
                return Ok(Vec::new());
            }
            *flag = true;
            Ok(vec![steering_for_queue])
        })
    }));
    cfg.prepare_request = Some(Arc::new(move |req, _sig| {
        let completed_for_req = Arc::clone(&completed_for_req);
        let steering_for_req = steering_for_req.clone();
        let canonical_for_req = canonical_for_req.clone();
        let replacement = replacement.clone();
        prepare_calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            assert!(completed_for_req.lock().unwrap().contains(&steering_for_req));
            assert!(req.context.messages.contains(&steering_for_req));
            Ok(Some(PrepareRequestResult {
                context: Some(AgentContext {
                    messages: vec![canonical_for_req],
                    tools: req.context.tools,
                }),
                model: Some(replacement),
                thinking_level: Some(ThinkingLevel::High),
            }))
        })
    }));

    let stream_fn: StreamFn = Arc::new(move |model, ctx, opts| {
        let expected_model = expected_model.clone();
        let canonical = canonical.clone();
        Box::pin(async move {
            assert_eq!(model.id, expected_model.id);
            assert_eq!(ctx.messages.len(), 1);
            assert_eq!(Some(&AgentMessage::from(ctx.messages[0].clone())), Some(&canonical));
            assert_eq!(opts.and_then(|o| o.reasoning), Some(pi_ai::ThinkingLevel::High));
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("done")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let emit: pi_agent_core::AgentEventSink = Arc::new(move |event| {
        if let AgentEvent::MessageEnd { message } = event {
            completed_for_emit.lock().unwrap().push(message);
        }
    });

    let _ = run_agent_loop(
        vec![user_msg("prompt")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        cfg,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .unwrap();

    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_poll_steering_after_prepare_request() {
    let queued = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));
    let late_steering = user_msg("late steering");
    let late_for_req = late_steering.clone();
    let late_for_stream = late_steering.clone();
    let included = Arc::new(Mutex::new(Vec::<bool>::new()));
    let preparations = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));

    let queued_for_poll = Arc::clone(&queued);
    let polls_ref = Arc::clone(&polls);
    let queued_for_req = Arc::clone(&queued);
    let prep_ref = Arc::clone(&preparations);
    let included_ref = Arc::clone(&included);

    let mut cfg = base_config();
    cfg.get_steering_messages = Some(Arc::new(move || {
        polls_ref.fetch_add(1, Ordering::SeqCst);
        let queued_for_poll = Arc::clone(&queued_for_poll);
        Box::pin(async move { Ok(std::mem::take(&mut *queued_for_poll.lock().unwrap())) })
    }));
    cfg.prepare_request = Some(Arc::new(move |_req, _sig| {
        let count = prep_ref.fetch_add(1, Ordering::SeqCst) + 1;
        let queued_for_req = Arc::clone(&queued_for_req);
        let late_for_req = late_for_req.clone();
        Box::pin(async move {
            if count == 1 {
                queued_for_req.lock().unwrap().push(late_for_req);
            }
            Ok(None)
        })
    }));

    let stream_fn: StreamFn = Arc::new(move |_m, ctx, _opts| {
        let included_ref = Arc::clone(&included_ref);
        let late_for_stream = late_for_stream.clone();
        Box::pin(async move {
            let has_late = ctx
                .messages
                .iter()
                .any(|m| Some(AgentMessage::from(m.clone())) == Some(late_for_stream.clone()));
            included_ref.lock().unwrap().push(has_late);
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("done")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });

    let stream = agent_loop(
        vec![user_msg("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(*included.lock().unwrap(), vec![false, true]);
    assert_eq!(preparations.load(Ordering::SeqCst), 2);
    assert_eq!(polls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn uses_prepare_next_turn_snapshot_and_picks_up_steering_queued_during_prepare() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));

    let second_has_guidance = Arc::new(Mutex::new(false));
    let second_has_steering = Arc::new(Mutex::new(false));
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let prepared = Arc::new(Mutex::new(false));
    let queued = Arc::new(Mutex::new(Vec::<AgentMessage>::new()));

    let pc_ref = Arc::clone(&prepare_calls);
    let prep_done = Arc::clone(&prepared);
    let queued_prep = Arc::clone(&queued);
    let queued_poll = Arc::clone(&queued);

    let mut cfg = base_config();
    cfg.prepare_next_turn_with_context = Some(Arc::new(move |ctx, _sig| {
        pc_ref.fetch_add(1, Ordering::SeqCst);
        let prep_done = Arc::clone(&prep_done);
        let queued_prep = Arc::clone(&queued_prep);
        Box::pin(async move {
            let mut done = prep_done.lock().unwrap();
            if *done {
                return Ok(None);
            }
            *done = true;
            queued_prep.lock().unwrap().push(user_msg("late steering"));
            let guidance = SystemMessage {
                content: SystemMessageContent::Text("updated guidance".to_string()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: 1,
            };
            Ok(Some(AgentLoopTurnUpdate {
                context: Some(AgentContext {
                    messages: ctx.context.messages,
                    tools: ctx.context.tools,
                }),
                messages: Some(vec![AgentMessage::from(guidance)]),
                model: None,
                thinking_level: None,
                reasoning: None,
            }))
        })
    }));
    cfg.get_steering_messages = Some(Arc::new(move || {
        let queued_poll = Arc::clone(&queued_poll);
        Box::pin(async move { Ok(std::mem::take(&mut *queued_poll.lock().unwrap())) })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_ref = Arc::clone(&llm_calls);
    let guidance_ref = Arc::clone(&second_has_guidance);
    let steering_ref = Arc::clone(&second_has_steering);

    let stream_fn: StreamFn = Arc::new(move |_m, ctx, _opts| {
        let idx = llm_ref.fetch_add(1, Ordering::SeqCst);
        let guidance_ref = Arc::clone(&guidance_ref);
        let steering_ref = Arc::clone(&steering_ref);
        Box::pin(async move {
            if idx == 1 {
                *guidance_ref.lock().unwrap() = ctx.messages.iter().any(|m| {
                    matches!(m, Message::System(SystemMessage { content: SystemMessageContent::Text(s), .. }) if s == "updated guidance")
                });
                *steering_ref.lock().unwrap() = ctx.messages.iter().any(|m| {
                    matches!(m, Message::User(UserMessage { content: UserMessageContent::Text(s), .. }) if s == "late steering")
                });
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let _ = collect_stream(stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert!(*second_has_guidance.lock().unwrap());
    assert!(*second_has_steering.lock().unwrap());
}

#[tokio::test]
async fn finish_turn_end_receives_finalized_turn_context_and_stops_before_queue_polling() {
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool = make_echo_tool(None, Arc::clone(&executed));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let seen_roles = Arc::new(Mutex::new(Vec::<String>::new()));

    let sp_ref = Arc::clone(&steering_polls);
    let fp_ref = Arc::clone(&follow_up_polls);
    let roles_ref = Arc::clone(&seen_roles);

    let mut cfg = base_config();
    cfg.finish_turn = Some(Arc::new(move |ctx, _sig| {
        let roles_ref = Arc::clone(&roles_ref);
        Box::pin(async move {
            assert_eq!(ctx.tool_results.len(), 1);
            assert_eq!(ctx.tool_results[0].tool_call_id, "tool-1");
            *roles_ref.lock().unwrap() = ctx
                .context
                .messages
                .iter()
                .filter_map(|m| m.role().map(str::to_string))
                .collect();
            Ok(Some(FinishTurnDecision {
                action: FinishTurnAction::End,
            }))
        })
    }));
    cfg.get_steering_messages = Some(Arc::new(move || {
        let sp_ref = Arc::clone(&sp_ref);
        Box::pin(async move {
            sp_ref.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        })
    }));
    cfg.get_follow_up_messages = Some(Arc::new(move || {
        let fp_ref = Arc::clone(&fp_ref);
        Box::pin(async move {
            fp_ref.fetch_add(1, Ordering::SeqCst);
            Ok(vec![user_msg("follow up should stay queued")])
        })
    }));

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_ref = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let idx = llm_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if idx == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("should not run")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });

    let stream = agent_loop(
        vec![user_msg("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool]),
        },
        cfg,
        None,
        Some(stream_fn),
    );
    let (_events, messages) = collect_stream(stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        *seen_roles.lock().unwrap(),
        vec!["system", "user", "assistant", "toolResult"]
    );
    assert_eq!(messages.len(), 4);
}

#[tokio::test]
async fn terminate_batch_rules_for_results_before_and_after_hooks() {
    // 1. Every tool result sets terminate = true -> stops after batch
    let executed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mut term_tool = make_echo_tool(None, Arc::clone(&executed));
    term_tool.execute = Arc::new(|_id, params, _sig, _on_update| {
        Box::pin(async move {
            let v = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
            let mut r = AgentToolResult::text(v);
            r.terminate = Some(true);
            Ok(r)
        })
    });
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_ref = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        llm_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(
                    vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            ))
        })
    });
    let stream = agent_loop(
        vec![user_msg("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![term_tool]),
        },
        base_config(),
        None,
        Some(stream_fn),
    );
    let (events, messages) = collect_stream(stream).await;
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        messages.iter().filter_map(AgentMessage::role).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "toolResult"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .count(),
        1
    );

    // 2. Blocked call with beforeToolCall.terminate = true -> stops
    let executed2 = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool2 = make_echo_tool(None, Arc::clone(&executed2));
    let mut cfg2 = base_config();
    cfg2.before_tool_call = Some(Arc::new(|_ctx, _sig| {
        Box::pin(async {
            Ok(Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some("Blocked by policy".to_string()),
                terminate: Some(true),
                args: None,
            }))
        })
    }));
    let llm_calls2 = Arc::new(AtomicUsize::new(0));
    let llm_ref2 = Arc::clone(&llm_calls2);
    let stream_fn2: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let count = llm_ref2.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if count == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("should not run")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });
    let stream2 = agent_loop(
        vec![user_msg("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool2]),
        },
        cfg2,
        None,
        Some(stream_fn2),
    );
    let (_events2, messages2) = collect_stream(stream2).await;
    assert!(executed2.lock().unwrap().is_empty());
    assert_eq!(llm_calls2.load(Ordering::SeqCst), 1);
    assert_eq!(
        tool_result_texts(&messages2),
        vec![("tool-1".to_string(), "Blocked by policy".to_string())]
    );

    // 3. Mixed batch with one terminating blocked call and one non-terminating call -> continues
    let executed3 = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool3 = make_echo_tool(None, Arc::clone(&executed3));
    let mut cfg3 = base_config();
    cfg3.tool_execution = Some(ToolExecutionMode::Parallel);
    cfg3.before_tool_call = Some(Arc::new(|ctx, _sig| {
        Box::pin(async move {
            let v = ctx.args.get("value").and_then(Value::as_str);
            if v == Some("first") {
                Ok(Some(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("Blocked first".to_string()),
                    terminate: Some(true),
                    args: None,
                }))
            } else {
                Ok(None)
            }
        })
    }));
    let llm_calls3 = Arc::new(AtomicUsize::new(0));
    let llm_ref3 = Arc::clone(&llm_calls3);
    let stream_fn3: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        let count = llm_ref3.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if count == 0 {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("tool-1", "echo", json!({ "value": "first" })),
                            tc_block("tool-2", "echo", json!({ "value": "second" })),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            } else {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            }
        })
    });
    let stream3 = agent_loop(
        vec![user_msg("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool3]),
        },
        cfg3,
        None,
        Some(stream_fn3),
    );
    let (_events3, messages3) = collect_stream(stream3).await;
    assert_eq!(*executed3.lock().unwrap(), vec![json!("second")]);
    assert_eq!(llm_calls3.load(Ordering::SeqCst), 2);
    assert_eq!(
        messages3.iter().filter_map(AgentMessage::role).collect::<Vec<_>>(),
        vec!["system", "user", "assistant", "toolResult", "toolResult", "assistant"]
    );

    // 4. afterToolCall sets terminate = true -> stops batch
    let executed4 = Arc::new(Mutex::new(Vec::<Value>::new()));
    let tool4 = make_echo_tool(None, Arc::clone(&executed4));
    let mut cfg4 = base_config();
    cfg4.after_tool_call = Some(Arc::new(|_ctx, _sig| {
        Box::pin(async {
            Ok(Some(AfterToolCallResult {
                terminate: Some(true),
                ..Default::default()
            }))
        })
    }));
    let llm_calls4 = Arc::new(AtomicUsize::new(0));
    let llm_ref4 = Arc::clone(&llm_calls4);
    let stream_fn4: StreamFn = Arc::new(move |_m, _ctx, _opts| {
        llm_ref4.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(
                    vec![tc_block("tool-1", "echo", json!({ "value": "hello" }))],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            ))
        })
    });
    let stream4 = agent_loop(
        vec![user_msg("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![tool4]),
        },
        cfg4,
        None,
        Some(stream_fn4),
    );
    let _ = collect_stream(stream4).await;
    assert_eq!(llm_calls4.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn agent_loop_continue_validation_and_custom_message_continuation() {
    let err = agent_loop_continue(
        AgentContext {
            messages: Vec::new(),
            tools: Some(Vec::new()),
        },
        base_config(),
        None,
        None,
    )
    .err()
    .expect("empty context must fail");
    assert!(err.message().contains("Cannot continue: no messages in context"));

    let stream_fn: StreamFn = Arc::new(|_m, _ctx, _opts| {
        Box::pin(async move {
            Ok(spawn_done_stream(
                assistant_msg(vec![text_block("Response")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });
    let stream = agent_loop_continue(
        AgentContext {
            messages: vec![user_msg("Hello")],
            tools: Some(Vec::new()),
        },
        base_config(),
        None,
        Some(Arc::clone(&stream_fn)),
    )
    .expect("continue should succeed");
    let (events, messages) = collect_stream(stream).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), Some("assistant"));
    let message_ends: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => Some(message.role()),
            _ => None,
        })
        .collect();
    assert_eq!(message_ends, vec![Some("assistant")]);

    let custom_msg = AgentMessage::custom(json!({
        "role": "custom",
        "text": "Hook content",
        "timestamp": now_ms()
    }));
    let mut cfg = base_config();
    cfg.convert_to_llm = Arc::new(|msgs| {
        Box::pin(async move {
            Ok(msgs
                .into_iter()
                .filter_map(|m| match m {
                    AgentMessage::Custom(val) if val.get("role").and_then(Value::as_str) == Some("custom") => {
                        Some(Message::User(UserMessage {
                            content: UserMessageContent::Text(
                                val.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                            ),
                            timestamp: val.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
                        }))
                    }
                    other => other.into_message(),
                })
                .collect())
        })
    });
    let stream2 = agent_loop_continue(
        AgentContext {
            messages: vec![custom_msg],
            tools: Some(Vec::new()),
        },
        cfg,
        None,
        Some(stream_fn),
    )
    .expect("continue with custom message should succeed");
    let (_events2, messages2) = collect_stream(stream2).await;
    assert_eq!(messages2.len(), 1);
    assert_eq!(messages2[0].role(), Some("assistant"));
}

#[tokio::test]
async fn run_tool_call_validates_runs_hooks_and_manages_structured_content() {
    let updates = Arc::new(Mutex::new(Vec::<AgentToolResult>::new()));
    let hook_calls = Arc::new(Mutex::new(Vec::<String>::new()));

    let echo = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo tool".to_string(),
        parameters: obj([("value", str())]),
        constrained_sampling: None,
        output_schema: Some(obj([("value", str())]).to_value()),
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, on_update| {
            Box::pin(async move {
                if let Some(cb) = on_update {
                    cb(AgentToolResult {
                        content: vec![UserContentBlock::Text(faux_text("partial"))],
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    });
                }
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                let text = value.as_str().unwrap_or("").to_string();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text(&text))],
                    details: Some(json!({})),
                    structured_content: Some(json!({ "value": value })),
                    usage: None,
                    is_error: None,
                    terminate: None,
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let failing = AgentTool {
        name: "failing".to_string(),
        label: "Failing".to_string(),
        description: "Returns an error result".to_string(),
        parameters: obj([] as [(&str, Schema); 0]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _on_update| {
            Box::pin(async {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text("bad"))],
                    details: Some(json!({ "partial": true })),
                    structured_content: None,
                    usage: None,
                    is_error: Some(true),
                    terminate: None,
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let assistant_message = assistant_msg(Vec::new(), StopReason::Stop);
    let hook_before = Arc::clone(&hook_calls);
    let hook_after = Arc::clone(&hook_calls);
    let updates_cb = Arc::clone(&updates);

    let options = RunToolCallOptions {
        tools: vec![echo.clone(), failing],
        assistant_message: assistant_message.clone(),
        context: AgentContext::default(),
        before_tool_call: Some(Arc::new(move |ctx, _sig| {
            let hook_before = Arc::clone(&hook_before);
            Box::pin(async move {
                hook_before.lock().unwrap().push(format!("before {}", ctx.tool_call.id));
                if ctx.args.get("value").and_then(Value::as_str) == Some("blocked") {
                    return Ok(Some(BeforeToolCallResult {
                        block: Some(true),
                        reason: Some("nope".to_string()),
                        terminate: None,
                        args: None,
                    }));
                }
                Ok(None)
            })
        })),
        after_tool_call: Some(Arc::new(move |ctx, _sig| {
            let hook_after = Arc::clone(&hook_after);
            Box::pin(async move {
                hook_after.lock().unwrap().push(format!("after {}", ctx.tool_call.id));
                Ok(None)
            })
        })),
        signal: None,
        on_update: Some(Arc::new(move |partial| {
            updates_cb.lock().unwrap().push(partial);
        })),
    };

    let a = run_tool_call(make_tool_call("a", "echo", json!({ "value": "a" })), options.clone()).await;
    assert_eq!(a.tool_call.id, "a");
    assert_eq!(a.result.structured_content, Some(json!({ "value": "a" })));
    assert!(!a.is_error);

    let b = run_tool_call(
        make_tool_call("b", "echo", json!({ "value": { "nested": true } })),
        options.clone(),
    )
    .await;
    assert!(b.is_error);

    let c = run_tool_call(
        make_tool_call("c", "echo", json!({ "value": "blocked" })),
        options.clone(),
    )
    .await;
    assert!(c.is_error);

    let d = run_tool_call(make_tool_call("d", "missing", json!({})), options.clone()).await;
    assert!(d.is_error);

    let e = run_tool_call(make_tool_call("e", "failing", json!({})), options).await;
    assert!(e.is_error);
    assert_eq!(e.result.details, Some(json!({ "partial": true })));
    assert_eq!(updates.lock().unwrap().len(), 1);
    assert_eq!(
        *hook_calls.lock().unwrap(),
        vec!["before a", "after a", "before c", "before e", "after e"]
    );

    let redacted = vec![UserContentBlock::Text(faux_text("redacted"))];
    let cases = vec![
        AfterToolCallResult {
            content: Some(redacted.clone()),
            ..Default::default()
        },
        AfterToolCallResult {
            structured_content: Some(json!({ "value": "replaced" })),
            ..Default::default()
        },
        AfterToolCallResult {
            content: Some(redacted),
            structured_content: Some(json!({ "value": "both" })),
            ..Default::default()
        },
        AfterToolCallResult {
            details: Some(json!({ "note": "kept" })),
            ..Default::default()
        },
    ];

    let mut seen = Vec::new();
    for after_res in cases {
        let outcome = run_tool_call(
            make_tool_call("x", "echo", json!({ "value": "original" })),
            RunToolCallOptions {
                tools: vec![echo.clone()],
                assistant_message: assistant_message.clone(),
                context: AgentContext::default(),
                before_tool_call: None,
                after_tool_call: Some(Arc::new(move |_ctx, _sig| {
                    let after_res = after_res.clone();
                    Box::pin(async move { Ok(Some(after_res)) })
                })),
                signal: None,
                on_update: None,
            },
        )
        .await;
        seen.push(outcome.result.structured_content);
    }

    assert_eq!(
        seen,
        vec![
            None,
            Some(json!({ "value": "replaced" })),
            Some(json!({ "value": "both" })),
            Some(json!({ "value": "original" })),
        ]
    );
}

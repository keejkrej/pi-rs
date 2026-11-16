//! Port of packages/agent/test/agent.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent_core::{
    AfterToolCallResult, Agent, AgentContext, AgentEvent, AgentInitialState, AgentLoopTurnUpdate, AgentMessage,
    AgentOptions, AgentTool, AgentToolResult, BeforeToolCallResult, FinishTurnAction, FinishTurnDecision,
    PrepareRequestResult, QueueMode, StreamFn, ThinkingBudgets, ThinkingLevel, ToolExecutionMode, Transport,
    set_default_stream_fn,
};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, FauxAssistantMessageContent, FauxToolCallOptions,
    JsonObject, Message, Model, StopReason, SystemMessage, SystemMessageContent, ToolReference, ToolResultMessage,
    UserContentBlock, UserMessage, UserMessageContent, create_assistant_message_event_stream, faux_assistant_message,
    faux_text, faux_tool_call, to_tool_declaration,
};
use pi_js::Error;
use pi_js::abort::AbortSignal;
use pi_js::time::now_ms;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};
use serial_test::serial;

struct DefaultStreamReset;

impl Drop for DefaultStreamReset {
    fn drop(&mut self) {
        set_default_stream_fn(None);
    }
}

#[derive(Clone)]
struct SignalGate {
    tx: tokio::sync::watch::Sender<bool>,
}

impl SignalGate {
    fn new() -> Self {
        let (tx, _) = tokio::sync::watch::channel(false);
        Self { tx }
    }

    fn open(&self) {
        let _ = self.tx.send(true);
    }

    async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|ready| *ready).await;
    }
}

fn create_model(id: &str) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": id,
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

fn user_text_from_agent_msg(msg: &AgentMessage) -> Option<String> {
    match msg {
        AgentMessage::Message(Message::User(u)) => match &u.content {
            UserMessageContent::Text(s) => Some(s.clone()),
            UserMessageContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            }),
        },
        _ => None,
    }
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => match &u.content {
                UserMessageContent::Text(s) => Some(s.clone()),
                UserMessageContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                }),
            },
            _ => None,
        })
        .collect()
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

fn assistant_error_msg(text: &str, stop_reason: StopReason) -> AssistantMessage {
    let mut msg = assistant_msg(vec![text_block(text)], stop_reason);
    msg.error_message = Some(text.to_string());
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

fn spawn_abortable_stream(
    started: Option<SignalGate>,
    signal: Option<AbortSignal>,
) -> pi_ai::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let stream_task = stream.clone();
    tokio::spawn(async move {
        stream_task.push(AssistantMessageEvent::Start {
            partial: assistant_msg(Vec::new(), StopReason::Pending),
        });
        if let Some(gate) = started {
            gate.open();
        }
        if let Some(sig) = signal {
            while !sig.is_aborted() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        } else {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let err = assistant_error_msg("Aborted", StopReason::Aborted);
        stream_task.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: err.clone(),
        });
        stream_task.end(Some(err));
    });
    stream
}

fn unused_stream_fn() -> StreamFn {
    Arc::new(|_model, _ctx, _opts| Box::pin(async move { Err(Error::msg("streamFn should not be called")) }))
}

fn make_noop_tool(name: &str) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        parameters: t::object(std::iter::empty::<(&str, _)>()),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _on_update| Box::pin(async { Ok(AgentToolResult::text("done")) })),
        replay: None,
    }
}

fn system_messages(messages: &[AgentMessage]) -> Vec<&SystemMessage> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Message(Message::System(sys)) => Some(sys),
            _ => None,
        })
        .collect()
}

fn message_roles(messages: &[AgentMessage]) -> Vec<&str> {
    messages.iter().filter_map(AgentMessage::role).collect()
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
                assistant_msg(vec![text_block("from default")], StopReason::Stop),
                StopReason::Stop,
            ))
        })
    });
    set_default_stream_fn(Some(default_fn));

    let agent = Agent::new(AgentOptions::default());
    agent.prompt("Hello").await.expect("prompt should succeed");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let state = agent.state();
    assert_eq!(state.messages.len(), 2);
    assert_eq!(message_roles(&state.messages), vec!["user", "assistant"]);
}

#[tokio::test]
async fn creates_agent_with_default_state() {
    let agent = Agent::new(AgentOptions::default());
    let state = agent.state();

    assert_eq!(state.system_prompt, "");
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.stream_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error.is_none());
}

#[tokio::test]
async fn creates_agent_with_custom_initial_state() {
    let custom_model = create_model("custom-model");
    let echo = make_noop_tool("custom_tool");
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(custom_model),
            thinking_level: Some(ThinkingLevel::Low),
            tools: Some(vec![echo]),
            messages: None,
        }),
        ..Default::default()
    });

    let state = agent.state();
    assert_eq!(state.system_prompt, "You are a helpful assistant.");
    assert_eq!(state.model.id, "custom-model");
    assert_eq!(state.thinking_level, ThinkingLevel::Low);
    assert_eq!(state.tools.len(), 1);
    assert_eq!(state.tools[0].name, "custom_tool");

    let sys = system_messages(&state.messages);
    assert_eq!(sys.len(), 1);
    assert_eq!(
        sys[0].content,
        SystemMessageContent::Text("You are a helpful assistant.".to_string())
    );
    let added: Vec<&str> = sys[0]
        .tools_added
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.name.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(added, vec!["custom_tool"]);
}

#[tokio::test]
async fn updates_state_with_mutators() {
    let agent = Agent::new(AgentOptions::default());

    agent.set_system_prompt("New prompt");
    assert_eq!(agent.state().system_prompt, "New prompt");

    let new_model = create_model("updated-model");
    agent.set_model(new_model);
    assert_eq!(agent.state().model.id, "updated-model");

    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(agent.state().thinking_level, ThinkingLevel::High);

    agent.set_tools(vec![make_noop_tool("tool_a")]);
    assert_eq!(agent.state().tools.len(), 1);
    assert_eq!(agent.state().tools[0].name, "tool_a");

    let new_messages = vec![user_msg("Hello")];
    agent.replace_messages(new_messages);
    assert_eq!(agent.state().messages.len(), 1);

    agent.append_message(AgentMessage::from(assistant_msg(
        vec![text_block("Hi")],
        StopReason::Stop,
    )));
    assert_eq!(agent.state().messages.len(), 2);

    agent.clear_messages();
    assert!(agent.state().messages.is_empty());

    agent.set_steering_mode(QueueMode::All);
    agent.set_follow_up_mode(QueueMode::All);
    agent.set_tool_execution(ToolExecutionMode::Sequential);
    agent.set_max_retry_delay_ms(Some(2500));
    agent.set_session_id(Some("session-42".to_string()));
    assert_eq!(agent.session_id().as_deref(), Some("session-42"));
}

#[tokio::test]
async fn emits_delta_system_messages_when_tools_change_between_prompts() {
    let seen_contexts = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let seen_ref = Arc::clone(&seen_contexts);

    let tool_a = make_noop_tool("tool_a");
    let tool_b = make_noop_tool("tool_b");

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Initial system".to_string()),
            tools: Some(vec![tool_a.clone()]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
            seen_ref.lock().unwrap().push(ctx.messages.clone());
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.prompt("turn 1").await.expect("turn 1 should succeed");

    agent.set_tools(vec![tool_b.clone()]);
    agent.prompt("turn 2").await.expect("turn 2 should succeed");

    let state = agent.state();
    let sys_msgs = system_messages(&state.messages);
    assert_eq!(sys_msgs.len(), 2);
    assert_eq!(
        sys_msgs[0]
            .tools_added
            .as_ref()
            .map(|v| v.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["tool_a"])
    );
    assert_eq!(
        sys_msgs[1]
            .tools_added
            .as_ref()
            .map(|v| v.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["tool_b"])
    );
    assert_eq!(
        sys_msgs[1].tools_removed,
        Some(vec![ToolReference {
            name: "tool_a".to_string(),
        }])
    );

    let captured = seen_contexts.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].iter().filter(|m| matches!(m, Message::System(_))).count(),
        1
    );
    assert_eq!(
        captured[1].iter().filter(|m| matches!(m, Message::System(_))).count(),
        2
    );
}

#[tokio::test]
async fn merges_tool_changes_into_pending_system_message_created_by_set_system_prompt() {
    let tool_a = make_noop_tool("tool_a");
    let tool_b = make_noop_tool("tool_b");

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Prompt 1".to_string()),
            tools: Some(vec![tool_a.clone()]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.prompt("turn 1").await.expect("turn 1 should succeed");

    agent.set_system_prompt("Prompt 2");
    agent.set_tools(vec![tool_a, tool_b]);
    agent.prompt("turn 2").await.expect("turn 2 should succeed");

    let state = agent.state();
    let sys_msgs = system_messages(&state.messages);
    assert_eq!(sys_msgs.len(), 2);
    assert_eq!(sys_msgs[1].content, SystemMessageContent::Text("Prompt 2".to_string()));
    assert_eq!(
        sys_msgs[1]
            .tools_added
            .as_ref()
            .map(|v| v.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["tool_b"])
    );
    assert!(sys_msgs[1].tools_removed.is_none());
}

#[tokio::test]
async fn rewrites_mismatched_pending_system_message_tool_declarations() {
    let tool_a = make_noop_tool("tool_a");
    let tool_b = make_noop_tool("tool_b");
    let stale_pending = SystemMessage {
        content: SystemMessageContent::Text("Updated prompt".to_string()),
        sections: None,
        tools_added: Some(vec![to_tool_declaration(&tool_a.tool)]),
        tools_removed: None,
        timestamp: now_ms(),
    };

    let seen_sys = Arc::new(Mutex::new(Vec::<SystemMessage>::new()));
    let seen_ref = Arc::clone(&seen_sys);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![tool_a, tool_b]),
            messages: Some(vec![
                user_msg("earlier"),
                AgentMessage::from(assistant_msg(vec![text_block("earlier reply")], StopReason::Stop)),
                AgentMessage::from(stale_pending),
            ]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
            let sys: Vec<SystemMessage> = ctx
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::System(s) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            *seen_ref.lock().unwrap() = sys;
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.prompt("follow up").await.expect("prompt should succeed");

    let state = agent.state();
    let sys_msgs = system_messages(&state.messages);
    assert_eq!(sys_msgs.len(), 1);
    let declared: Vec<&str> = sys_msgs[0]
        .tools_added
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.name.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(declared, vec!["tool_a", "tool_b"]);

    let sent = seen_sys.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]
            .tools_added
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["tool_a", "tool_b"])
    );
}

#[tokio::test]
async fn reset_restores_baseline_system_message_and_clears_transient_state() {
    let tool = make_noop_tool("helper");
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Baseline system".to_string()),
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.prompt("Hello").await.expect("prompt should succeed");
    agent.steer(user_msg("queued steering"));
    agent.follow_up(user_msg("queued followUp"));
    assert!(agent.state().messages.len() >= 3);
    assert!(agent.has_queued_messages());

    agent.reset().expect("reset when idle should succeed");
    let state = agent.state();
    assert_eq!(state.system_prompt, "Baseline system");
    assert_eq!(state.messages.len(), 1);
    assert_eq!(message_roles(&state.messages), vec!["system"]);
    assert!(!state.is_streaming);
    assert!(state.stream_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error.is_none());
    assert!(!agent.has_queued_messages());
}

#[tokio::test]
async fn rejects_reset_while_processing_without_corrupting_transcript() {
    let stream_started = SignalGate::new();
    let started_ref = stream_started.clone();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Baseline system".to_string()),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_m, _ctx, opts| {
            let signal = opts.and_then(|o| o.signal);
            let started_ref = started_ref.clone();
            Box::pin(async move { Ok(spawn_abortable_stream(Some(started_ref), signal)) })
        })),
        ..Default::default()
    });

    let agent_ref = Arc::new(agent);
    let prompt_handle = {
        let agent = Arc::clone(&agent_ref);
        tokio::spawn(async move { agent.prompt("First prompt").await })
    };

    stream_started.wait().await;
    assert!(agent_ref.state().is_streaming);

    let err = agent_ref.reset().expect_err("reset while streaming must fail");
    assert!(err.message().contains("Cannot reset while processing"));
    assert!(agent_ref.state().is_streaming);
    assert_eq!(message_roles(&agent_ref.state().messages), vec!["system", "user"]);

    agent_ref.abort();
    prompt_handle.await.unwrap().expect("prompt settles cleanly on abort");
    assert!(!agent_ref.state().is_streaming);
}

#[tokio::test]
async fn subscribes_and_unsubscribes_sync_listeners() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("Response")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    let count = Arc::new(AtomicUsize::new(0));
    let count_ref = Arc::clone(&count);
    let unsub = agent.subscribe(move |_ev| {
        count_ref.fetch_add(1, Ordering::SeqCst);
    });

    agent.prompt("first").await.expect("first prompt succeeds");
    let first_count = count.load(Ordering::SeqCst);
    assert!(first_count > 0);

    unsub();
    agent.prompt("second").await.expect("second prompt succeeds");
    assert_eq!(count.load(Ordering::SeqCst), first_count);
}

#[tokio::test]
async fn emits_full_lifecycle_events_for_thrown_run_failures() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move { Err(Error::msg("Stream boom")) })
        })),
        ..Default::default()
    });

    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_ref = Arc::clone(&events);
    let _unsub = agent.subscribe(move |ev| {
        events_ref.lock().unwrap().push(ev.clone());
    });

    agent
        .prompt("Hello")
        .await
        .expect("prompt catches run failure internally");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.stream_message.is_none());
    assert_eq!(state.error.as_deref(), Some("Stream boom"));
    assert_eq!(message_roles(&state.messages), vec!["user", "assistant"]);

    let captured = events.lock().unwrap();
    let event_tags: Vec<&str> = captured
        .iter()
        .map(|ev| match ev {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::MessageStart { message } => match message.role() {
                Some("user") => "message_start:user",
                Some("assistant") => "message_start:assistant",
                _ => "message_start:other",
            },
            AgentEvent::MessageEnd { message } => match message.role() {
                Some("user") => "message_end:user",
                Some("assistant") => "message_end:assistant",
                _ => "message_end:other",
            },
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::AgentEnd { .. } => "agent_end",
            _ => "other",
        })
        .collect();

    assert_eq!(
        event_tags,
        vec![
            "agent_start",
            "turn_start",
            "message_start:user",
            "message_end:user",
            "message_start:assistant",
            "message_end:assistant",
            "turn_end",
            "agent_end",
        ]
    );
}

#[tokio::test]
async fn awaits_async_subscribers_before_prompt_resolves() {
    let agent = Arc::new(Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    }));

    let barrier = SignalGate::new();
    let barrier_wait = barrier.clone();
    let listener_finished = Arc::new(AtomicBool::new(false));
    let listener_ref = Arc::clone(&listener_finished);

    let _unsub = agent.subscribe_async(move |ev, _sig| {
        let barrier_wait = barrier_wait.clone();
        let listener_ref = Arc::clone(&listener_ref);
        Box::pin(async move {
            if matches!(ev, AgentEvent::AgentEnd { .. }) {
                barrier_wait.wait().await;
                listener_ref.store(true, Ordering::SeqCst);
            }
        })
    });

    let prompt_resolved = Arc::new(AtomicBool::new(false));
    let resolved_ref = Arc::clone(&prompt_resolved);
    let agent_task = Arc::clone(&agent);
    let prompt_handle = tokio::spawn(async move {
        let res = agent_task.prompt("hello").await;
        resolved_ref.store(true, Ordering::SeqCst);
        res
    });

    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(!prompt_resolved.load(Ordering::SeqCst));
    assert!(!listener_finished.load(Ordering::SeqCst));
    assert!(agent.state().is_streaming);

    barrier.open();
    prompt_handle.await.unwrap().expect("prompt should finish");

    assert!(listener_finished.load(Ordering::SeqCst));
    assert!(prompt_resolved.load(Ordering::SeqCst));
    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn wait_for_idle_waits_for_async_subscribers() {
    let agent = Arc::new(Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    }));

    let barrier = SignalGate::new();
    let barrier_wait = barrier.clone();
    let _unsub = agent.subscribe_async(move |ev, _sig| {
        let barrier_wait = barrier_wait.clone();
        Box::pin(async move {
            if let AgentEvent::MessageEnd { message } = ev
                && message.role() == Some("assistant")
            {
                barrier_wait.wait().await;
            }
        })
    });

    let agent_prompt = Arc::clone(&agent);
    let prompt_handle = tokio::spawn(async move { agent_prompt.prompt("hello").await });
    tokio::time::sleep(Duration::from_millis(5)).await;

    let idle_resolved = Arc::new(AtomicBool::new(false));
    let idle_ref = Arc::clone(&idle_resolved);
    let agent_idle = Arc::clone(&agent);
    let idle_handle = tokio::spawn(async move {
        agent_idle.wait_for_idle().await;
        idle_ref.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(!idle_resolved.load(Ordering::SeqCst));
    assert!(agent.state().is_streaming);

    barrier.open();
    prompt_handle.await.unwrap().expect("prompt should complete");
    idle_handle.await.unwrap();

    assert!(idle_resolved.load(Ordering::SeqCst));
    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn passes_active_abort_signal_to_subscribers_and_exposes_signal() {
    let stream_started = SignalGate::new();
    let started_ref = stream_started.clone();

    let agent = Arc::new(Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_m, _ctx, opts| {
            let signal = opts.and_then(|o| o.signal);
            let started_ref = started_ref.clone();
            Box::pin(async move { Ok(spawn_abortable_stream(Some(started_ref), signal)) })
        })),
        ..Default::default()
    }));

    let received_signal: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let sig_ref = Arc::clone(&received_signal);
    let _unsub = agent.subscribe_async(move |ev, signal| {
        let sig_ref = Arc::clone(&sig_ref);
        Box::pin(async move {
            if matches!(ev, AgentEvent::AgentStart) {
                *sig_ref.lock().unwrap() = Some(signal);
            }
        })
    });

    let agent_task = Arc::clone(&agent);
    let prompt_handle = tokio::spawn(async move { agent_task.prompt("hello").await });

    stream_started.wait().await;
    let active_sig = received_signal.lock().unwrap().clone().expect("signal captured");
    assert!(!active_sig.is_aborted());
    assert!(agent.signal().is_some());

    agent.abort();
    prompt_handle.await.unwrap().expect("prompt settles cleanly");

    assert!(active_sig.is_aborted());
    assert!(agent.signal().is_none());
}

#[tokio::test]
async fn ignores_tool_updates_after_tool_execution_settles() {
    type UpdateCb = pi_agent_core::AgentToolUpdateCallback;
    let delayed_update: Arc<Mutex<Option<UpdateCb>>> = Arc::new(Mutex::new(None));
    let delayed_capture = Arc::clone(&delayed_update);

    let tool = AgentTool {
        name: "delayed_tool".to_string(),
        label: "Delayed Tool".to_string(),
        description: "Captures progress callbacks".to_string(),
        parameters: t::object(std::iter::empty::<(&str, _)>()),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, on_update| {
            let delayed_capture = Arc::clone(&delayed_capture);
            Box::pin(async move {
                *delayed_capture.lock().unwrap() = on_update.clone();
                if let Some(cb) = on_update {
                    cb(AgentToolResult {
                        content: vec![UserContentBlock::Text(faux_text("running"))],
                        details: Some(json!({ "status": "running" })),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                        control: None,
                    });
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text("ok"))],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![tc_block("call-1", "delayed_tool", json!({}))], StopReason::ToolUse),
                    StopReason::ToolUse,
                ))
            })
        })),
        ..Default::default()
    });

    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_ref = Arc::clone(&events);
    let _unsub = agent.subscribe(move |ev| {
        events_ref.lock().unwrap().push(ev.clone());
    });

    agent.prompt("run tool").await.expect("prompt succeeds");
    let event_count_after_prompt = events.lock().unwrap().len();

    let captured_cb = delayed_update.lock().unwrap().take().expect("on_update captured");
    captured_cb(AgentToolResult {
        content: vec![UserContentBlock::Text(faux_text("late"))],
        details: Some(json!({ "status": "late" })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
        control: None,
    });
    tokio::time::sleep(Duration::from_millis(10)).await;

    let captured_events = events.lock().unwrap();
    let update_count = captured_events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
        .count();
    assert_eq!(update_count, 1);
    assert_eq!(captured_events.len(), event_count_after_prompt);
}

#[tokio::test]
async fn ignores_settled_parallel_tool_update_while_another_tool_is_still_running() {
    type UpdateCb = pi_agent_core::AgentToolUpdateCallback;
    let slow_started = SignalGate::new();
    let settled_ended = SignalGate::new();
    let release_slow = SignalGate::new();

    let settled_cb: Arc<Mutex<Option<UpdateCb>>> = Arc::new(Mutex::new(None));
    let settled_capture = Arc::clone(&settled_cb);

    let settled_tool = AgentTool {
        name: "settled_tool".to_string(),
        label: "Settled Tool".to_string(),
        description: "Captures progress callbacks".to_string(),
        parameters: t::object(std::iter::empty::<(&str, _)>()),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, on_update| {
            let settled_capture = Arc::clone(&settled_capture);
            Box::pin(async move {
                *settled_capture.lock().unwrap() = on_update;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text("done"))],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let slow_started_tx = slow_started.clone();
    let release_slow_rx = release_slow.clone();
    let slow_tool = AgentTool {
        name: "slow_tool".to_string(),
        label: "Slow Tool".to_string(),
        description: "Keeps the agent run active".to_string(),
        parameters: t::object(std::iter::empty::<(&str, _)>()),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _on_update| {
            let slow_started_tx = slow_started_tx.clone();
            let release_slow_rx = release_slow_rx.clone();
            Box::pin(async move {
                slow_started_tx.open();
                release_slow_rx.wait().await;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(faux_text("done"))],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                    control: None,
                })
            })
        }),
        replay: None,
    };

    let agent = Arc::new(Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![settled_tool, slow_tool]),
            ..Default::default()
        }),
        tool_execution: Some(ToolExecutionMode::Parallel),
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(
                        vec![
                            tc_block("call-1", "settled_tool", json!({})),
                            tc_block("call-2", "slow_tool", json!({})),
                        ],
                        StopReason::ToolUse,
                    ),
                    StopReason::ToolUse,
                ))
            })
        })),
        ..Default::default()
    }));

    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_ref = Arc::clone(&events);
    let settled_ended_tx = settled_ended.clone();
    let _unsub = agent.subscribe(move |ev| {
        events_ref.lock().unwrap().push(ev.clone());
        if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = ev
            && tool_call_id == "call-1"
        {
            settled_ended_tx.open();
        }
    });

    let agent_task = Arc::clone(&agent);
    let prompt_handle = tokio::spawn(async move { agent_task.prompt("run tools").await });

    slow_started.wait().await;
    settled_ended.wait().await;
    let count_before_late = events.lock().unwrap().len();

    let cb = settled_cb.lock().unwrap().take().expect("on_update captured");
    cb(AgentToolResult {
        content: vec![UserContentBlock::Text(faux_text("late"))],
        details: Some(json!({ "status": "late" })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
        control: None,
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(events.lock().unwrap().len(), count_before_late);

    release_slow.open();
    prompt_handle.await.unwrap().expect("prompt succeeds");

    let update_count = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
        .count();
    assert_eq!(update_count, 0);
}

#[tokio::test]
async fn supports_steering_and_follow_up_message_queues() {
    let agent = Agent::new(AgentOptions::default());

    assert!(!agent.has_queued_messages());
    assert!(agent.peek_queued_messages().is_empty());

    let steer_msg = user_msg("Steering message");
    agent.steer(steer_msg);
    assert!(agent.has_queued_messages());
    assert_eq!(agent.peek_queued_messages().len(), 1);
    assert_eq!(
        user_text_from_agent_msg(&agent.peek_queued_messages()[0]).as_deref(),
        Some("Steering message")
    );

    let follow_msg = user_msg("Follow-up message");
    agent.follow_up(follow_msg);
    // peek_queued_messages returns steering first when steering is non-empty
    assert_eq!(
        user_text_from_agent_msg(&agent.peek_queued_messages()[0]).as_deref(),
        Some("Steering message")
    );

    agent.clear_steering_queue();
    assert_eq!(
        user_text_from_agent_msg(&agent.peek_queued_messages()[0]).as_deref(),
        Some("Follow-up message")
    );

    agent.clear_follow_up_queue();
    assert!(!agent.has_queued_messages());

    agent.steer(user_msg("s"));
    agent.follow_up(user_msg("f"));
    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

#[tokio::test]
async fn handles_abort_cleanly() {
    let stream_started = SignalGate::new();
    let started_ref = stream_started.clone();

    let agent = Arc::new(Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_m, _ctx, opts| {
            let signal = opts.and_then(|o| o.signal);
            let started_ref = started_ref.clone();
            Box::pin(async move { Ok(spawn_abortable_stream(Some(started_ref), signal)) })
        })),
        ..Default::default()
    }));

    let agent_task = Arc::clone(&agent);
    let handle = tokio::spawn(async move { agent_task.prompt("start").await });

    stream_started.wait().await;
    agent.abort();
    handle.await.unwrap().expect("abort settles cleanly");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.error.as_deref(), Some("Aborted"));
}

#[tokio::test]
async fn throws_when_prompt_or_continue_called_while_streaming() {
    let stream_started = SignalGate::new();
    let started_ref = stream_started.clone();

    let agent = Arc::new(Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_m, _ctx, opts| {
            let signal = opts.and_then(|o| o.signal);
            let started_ref = started_ref.clone();
            Box::pin(async move { Ok(spawn_abortable_stream(Some(started_ref), signal)) })
        })),
        ..Default::default()
    }));

    let agent_task = Arc::clone(&agent);
    let first_prompt = tokio::spawn(async move { agent_task.prompt("First message").await });

    stream_started.wait().await;
    assert!(agent.state().is_streaming);

    let prompt_err = agent
        .prompt("Second message")
        .await
        .expect_err("concurrent prompt must fail");
    assert!(prompt_err.message().contains("Agent is already processing a prompt"));

    let cont_err = agent.continue_run().await.expect_err("concurrent continue must fail");
    assert!(cont_err.message().contains("Agent is already processing"));

    agent.abort();
    first_prompt.await.unwrap().expect("aborted prompt settles");
}

#[tokio::test]
async fn continue_processes_queued_follow_up_messages_after_assistant_turn() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("Processed")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.replace_messages(vec![
        user_msg("Initial"),
        AgentMessage::from(assistant_msg(vec![text_block("Initial response")], StopReason::Stop)),
    ]);

    agent.follow_up(AgentMessage::from(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(faux_text("Queued follow-up"))]),
        timestamp: now_ms(),
    }));

    agent.continue_run().await.expect("continue should succeed");

    let messages = agent.state().messages;
    assert!(
        messages
            .iter()
            .any(|m| user_text_from_agent_msg(m).as_deref() == Some("Queued follow-up"))
    );
    assert_eq!(messages.last().and_then(AgentMessage::role), Some("assistant"));
}

#[tokio::test]
async fn continue_keeps_one_at_a_time_and_all_steering_semantics_for_assistant_tail() {
    for (mode, expected_requests) in [(QueueMode::OneAtATime, 2usize), (QueueMode::All, 1usize)] {
        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let req_ref = Arc::clone(&requests);

        let agent = Agent::new(AgentOptions {
            steering_mode: Some(mode),
            stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
                req_ref.lock().unwrap().push(user_texts(&ctx.messages));
                Box::pin(async move {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![text_block("Processed")], StopReason::Stop),
                        StopReason::Stop,
                    ))
                })
            })),
            ..Default::default()
        });

        agent.replace_messages(vec![
            user_msg("Initial"),
            AgentMessage::from(assistant_msg(vec![text_block("Initial response")], StopReason::Stop)),
        ]);
        agent.steer(user_msg("Steering 1"));
        agent.steer(user_msg("Steering 2"));

        agent.continue_run().await.expect("continue should succeed");

        let seen = requests.lock().unwrap();
        assert_eq!(seen.len(), expected_requests);
        assert!(seen[0].contains(&"Steering 1".to_string()));
        if mode == QueueMode::OneAtATime {
            assert!(!seen[0].contains(&"Steering 2".to_string()));
            assert!(seen[1].contains(&"Steering 2".to_string()));
        } else {
            assert!(seen[0].contains(&"Steering 2".to_string()));
        }
    }
}

#[tokio::test]
async fn rejects_queued_continuation_from_empty_and_system_only_context_without_draining_queues() {
    let sys_only = SystemMessage {
        content: SystemMessageContent::Text("system only".to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp: 1,
    };

    for initial_messages in [Vec::new(), vec![AgentMessage::from(sys_only)]] {
        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                messages: Some(initial_messages),
                ..Default::default()
            }),
            stream_fn: Some(unused_stream_fn()),
            ..Default::default()
        });
        agent.steer(user_msg("steering"));
        agent.follow_up(user_msg("follow-up"));

        let err = agent
            .continue_run()
            .await
            .expect_err("must reject empty/system-only continue");
        assert!(err.message().contains("No messages to continue from"));

        let queued_steering = agent.peek_queued_messages();
        assert_eq!(queued_steering.len(), 1);
        assert_eq!(
            user_text_from_agent_msg(&queued_steering[0]).as_deref(),
            Some("steering")
        );

        agent.clear_steering_queue();
        let queued_follow_up = agent.peek_queued_messages();
        assert_eq!(queued_follow_up.len(), 1);
        assert_eq!(
            user_text_from_agent_msg(&queued_follow_up[0]).as_deref(),
            Some("follow-up")
        );
    }
}

#[tokio::test]
async fn defers_follow_up_input_on_first_continuation_request_from_user_and_tool_result_tails() {
    let tr: ToolResultMessage = serde_json::from_value(json!({
        "role": "toolResult",
        "toolCallId": "call-1",
        "toolName": "noop",
        "content": [{ "type": "text", "text": "done" }],
        "isError": false,
        "timestamp": 1
    }))
    .expect("valid toolResult json");

    let tails = vec![
        vec![user_msg("existing user")],
        vec![
            user_msg("existing user"),
            AgentMessage::from(assistant_msg(
                vec![tc_block("call-1", "noop", json!({}))],
                StopReason::ToolUse,
            )),
            AgentMessage::from(tr),
        ],
    ];

    for messages in tails {
        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let req_ref = Arc::clone(&requests);

        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                messages: Some(messages),
                ..Default::default()
            }),
            stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
                req_ref.lock().unwrap().push(user_texts(&ctx.messages));
                Box::pin(async move {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![text_block("done")], StopReason::Stop),
                        StopReason::Stop,
                    ))
                })
            })),
            ..Default::default()
        });
        agent.follow_up(user_msg("follow-up"));

        agent.continue_run().await.expect("continue succeeds");

        let seen = requests.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(!seen[0].contains(&"follow-up".to_string()));
        assert!(seen[1].contains(&"follow-up".to_string()));
    }
}

#[tokio::test]
async fn polls_one_at_a_time_and_all_steering_at_continuation_startup() {
    for (mode, expected_requests) in [(QueueMode::OneAtATime, 2usize), (QueueMode::All, 1usize)] {
        let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let req_ref = Arc::clone(&requests);

        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                messages: Some(vec![user_msg("existing")]),
                ..Default::default()
            }),
            steering_mode: Some(mode),
            stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
                req_ref.lock().unwrap().push(user_texts(&ctx.messages));
                Box::pin(async move {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![text_block("done")], StopReason::Stop),
                        StopReason::Stop,
                    ))
                })
            })),
            ..Default::default()
        });
        agent.steer(user_msg("first"));
        agent.steer(user_msg("second"));

        agent.continue_run().await.expect("continue succeeds");

        let seen = requests.lock().unwrap();
        assert_eq!(seen.len(), expected_requests);
        assert!(seen[0].contains(&"first".to_string()));
        if mode == QueueMode::OneAtATime {
            assert!(!seen[0].contains(&"second".to_string()));
            assert!(seen[1].contains(&"second".to_string()));
        } else {
            assert!(seen[0].contains(&"second".to_string()));
        }
    }
}

#[tokio::test]
async fn keeps_steering_ahead_of_follow_up_from_non_assistant_continuation_tail() {
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let req_ref = Arc::clone(&requests);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![user_msg("existing")]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_m, ctx, _opts| {
            req_ref.lock().unwrap().push(user_texts(&ctx.messages));
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });
    agent.steer(user_msg("steering"));
    agent.follow_up(user_msg("follow-up"));

    agent.continue_run().await.expect("continue succeeds");

    let seen = requests.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].contains(&"steering".to_string()));
    assert!(!seen[0].contains(&"follow-up".to_string()));
    assert!(seen[1].contains(&"follow-up".to_string()));
}

#[tokio::test]
async fn keeps_queues_on_error_and_aborted_responses_even_when_finish_turn_requests_continuation() {
    for stop_reason in [StopReason::Error, StopReason::Aborted] {
        let queued_during_response = user_msg("steering");
        let follow_up = user_msg("follow-up");

        let agent = Arc::new(Agent::new(AgentOptions {
            finish_turn: Some(Arc::new(|_ctx, _sig| {
                Box::pin(async {
                    Ok(Some(FinishTurnDecision {
                        action: FinishTurnAction::Continue,
                    }))
                })
            })),
            stream_fn: Some(Arc::new(move |_m, _ctx, _opts| {
                Box::pin(async move {
                    let msg = assistant_error_msg("failed", stop_reason);
                    Ok(spawn_error_stream(msg, stop_reason))
                })
            })),
            ..Default::default()
        }));

        agent.follow_up(follow_up);
        let agent_for_sub = Arc::clone(&agent);
        let steer_clone = queued_during_response.clone();
        let _unsub = agent.subscribe(move |ev| {
            if let AgentEvent::MessageEnd { message } = ev
                && message.role() == Some("assistant")
            {
                agent_for_sub.steer(steer_clone.clone());
            }
        });

        agent.prompt("start").await.expect("prompt settles cleanly");

        let first = agent.peek_queued_messages();
        assert_eq!(first.len(), 1);
        assert_eq!(user_text_from_agent_msg(&first[0]).as_deref(), Some("steering"));
        agent.clear_steering_queue();

        let second = agent.peek_queued_messages();
        assert_eq!(second.len(), 1);
        assert_eq!(user_text_from_agent_msg(&second[0]).as_deref(), Some("follow-up"));
    }
}

#[tokio::test]
async fn keeps_queues_when_finish_turn_ends_run() {
    let agent = Arc::new(Agent::new(AgentOptions {
        finish_turn: Some(Arc::new(|_ctx, _sig| {
            Box::pin(async {
                Ok(Some(FinishTurnDecision {
                    action: FinishTurnAction::End,
                }))
            })
        })),
        stream_fn: Some(Arc::new(|_m, _ctx, _opts| {
            Box::pin(async move {
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("done")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    }));

    agent.follow_up(user_msg("follow-up"));
    let agent_for_sub = Arc::clone(&agent);
    let _unsub = agent.subscribe(move |ev| {
        if let AgentEvent::MessageEnd { message } = ev
            && message.role() == Some("assistant")
        {
            agent_for_sub.steer(user_msg("steering"));
        }
    });

    agent.prompt("start").await.expect("prompt settles cleanly");

    let first = agent.peek_queued_messages();
    assert_eq!(first.len(), 1);
    assert_eq!(user_text_from_agent_msg(&first[0]).as_deref(), Some("steering"));
    agent.clear_steering_queue();

    let second = agent.peek_queued_messages();
    assert_eq!(second.len(), 1);
    assert_eq!(user_text_from_agent_msg(&second[0]).as_deref(), Some("follow-up"));
}

#[tokio::test]
async fn keeps_legacy_prepare_next_turn_signal_callback_behavior() {
    let requests = Arc::new(AtomicUsize::new(0));
    let saw_signal = Arc::new(AtomicBool::new(false));
    let req_ref = Arc::clone(&requests);
    let sig_ref = Arc::clone(&saw_signal);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![make_noop_tool("noop")]),
            ..Default::default()
        }),
        prepare_next_turn: Some(Arc::new(move |sig| {
            let sig_ref = Arc::clone(&sig_ref);
            Box::pin(async move {
                sig_ref.store(sig.is_some(), Ordering::SeqCst);
                Ok(None)
            })
        })),
        stream_fn: Some(Arc::new(move |_m, _ctx, _opts| {
            let n = req_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
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
        })),
        ..Default::default()
    });

    agent.prompt("start").await.expect("prompt should succeed");

    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(saw_signal.load(Ordering::SeqCst));
}

#[tokio::test]
async fn forwards_finish_turn_through_agent_options_with_active_abort_signal() {
    let requests = Arc::new(AtomicUsize::new(0));
    let saw_signal = Arc::new(AtomicBool::new(false));
    let callback_roles = Arc::new(Mutex::new(Vec::<String>::new()));

    let req_ref = Arc::clone(&requests);
    let sig_ref = Arc::clone(&saw_signal);
    let roles_ref = Arc::clone(&callback_roles);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![make_noop_tool("noop")]),
            ..Default::default()
        }),
        finish_turn: Some(Arc::new(move |ctx, sig| {
            let sig_ref = Arc::clone(&sig_ref);
            let roles_ref = Arc::clone(&roles_ref);
            Box::pin(async move {
                sig_ref.store(sig.is_some(), Ordering::SeqCst);
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
        })),
        stream_fn: Some(Arc::new(move |_m, _ctx, _opts| {
            let n = req_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![tc_block("tool-1", "noop", json!({}))], StopReason::ToolUse),
                        StopReason::ToolUse,
                    ))
                } else {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![text_block("should not run")], StopReason::Stop),
                        StopReason::Stop,
                    ))
                }
            })
        })),
        ..Default::default()
    });

    agent.prompt("start").await.expect("prompt should succeed");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(saw_signal.load(Ordering::SeqCst));
    assert_eq!(
        *callback_roles.lock().unwrap(),
        vec!["system", "user", "assistant", "toolResult"]
    );
}

#[tokio::test]
async fn forwards_stream_options_hooks_and_metadata_through_agent_options() {
    let seen_session_id = Arc::new(Mutex::new(None::<String>));
    let seen_api_key = Arc::new(Mutex::new(None::<String>));
    let seen_max_retry = Arc::new(Mutex::new(None::<i64>));
    let seen_has_budgets = Arc::new(AtomicBool::new(false));
    let seen_has_transport = Arc::new(AtomicBool::new(false));

    let sid_ref = Arc::clone(&seen_session_id);
    let key_ref = Arc::clone(&seen_api_key);
    let retry_ref = Arc::clone(&seen_max_retry);
    let budget_ref = Arc::clone(&seen_has_budgets);
    let transport_ref = Arc::clone(&seen_has_transport);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            model: Some(create_model("forward-model")),
            thinking_level: Some(ThinkingLevel::Medium),
            ..Default::default()
        }),
        session_id: Some("sess-99".to_string()),
        get_api_key: Some(Arc::new(|provider| {
            Box::pin(async move { Ok(Some(format!("key-for-{provider}"))) })
        })),
        thinking_budgets: Some(ThinkingBudgets::default()),
        transport: Some(Transport::default()),
        max_retry_delay_ms: Some(1234),
        stream_fn: Some(Arc::new(move |_m, _ctx, opts| {
            let sid_ref = Arc::clone(&sid_ref);
            let key_ref = Arc::clone(&key_ref);
            let retry_ref = Arc::clone(&retry_ref);
            let budget_ref = Arc::clone(&budget_ref);
            let transport_ref = Arc::clone(&transport_ref);
            Box::pin(async move {
                if let Some(o) = opts {
                    *sid_ref.lock().unwrap() = o.session_id;
                    *key_ref.lock().unwrap() = o.api_key;
                    *retry_ref.lock().unwrap() = o.max_retry_delay_ms;
                    budget_ref.store(o.thinking_budgets.is_some(), Ordering::SeqCst);
                    transport_ref.store(o.transport.is_some(), Ordering::SeqCst);
                }
                Ok(spawn_done_stream(
                    assistant_msg(vec![text_block("ok")], StopReason::Stop),
                    StopReason::Stop,
                ))
            })
        })),
        ..Default::default()
    });

    agent.prompt("check options").await.expect("prompt succeeds");

    assert_eq!(seen_session_id.lock().unwrap().as_deref(), Some("sess-99"));
    assert_eq!(seen_api_key.lock().unwrap().as_deref(), Some("key-for-openai"));
    assert_eq!(*seen_max_retry.lock().unwrap(), Some(1234));
    assert!(seen_has_budgets.load(Ordering::SeqCst));
    assert!(seen_has_transport.load(Ordering::SeqCst));
}

#[tokio::test]
async fn forwards_tool_and_turn_hooks_through_agent_options() {
    let before_called = Arc::new(AtomicBool::new(false));
    let after_called = Arc::new(AtomicBool::new(false));
    let next_turn_called = Arc::new(AtomicBool::new(false));
    let prepare_req_called = Arc::new(AtomicBool::new(false));
    let seen_model_ids = Arc::new(Mutex::new(Vec::<String>::new()));

    let before_ref = Arc::clone(&before_called);
    let after_ref = Arc::clone(&after_called);
    let next_ref = Arc::clone(&next_turn_called);
    let prep_ref = Arc::clone(&prepare_req_called);
    let models_ref = Arc::clone(&seen_model_ids);

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_ref = Arc::clone(&llm_calls);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            model: Some(create_model("initial-model")),
            tools: Some(vec![make_noop_tool("noop")]),
            ..Default::default()
        }),
        before_tool_call: Some(Arc::new(move |_ctx, _sig| {
            let before_ref = Arc::clone(&before_ref);
            Box::pin(async move {
                before_ref.store(true, Ordering::SeqCst);
                Ok(Some(BeforeToolCallResult {
                    block: None,
                    reason: None,
                    terminate: None,
                    args: Some(json!({})),
                }))
            })
        })),
        after_tool_call: Some(Arc::new(move |_ctx, _sig| {
            let after_ref = Arc::clone(&after_ref);
            Box::pin(async move {
                after_ref.store(true, Ordering::SeqCst);
                Ok(Some(AfterToolCallResult {
                    details: Some(json!({ "audited": true })),
                    ..Default::default()
                }))
            })
        })),
        prepare_next_turn_with_context: Some(Arc::new(move |ctx, _sig| {
            let next_ref = Arc::clone(&next_ref);
            Box::pin(async move {
                next_ref.store(true, Ordering::SeqCst);
                Ok(Some(AgentLoopTurnUpdate {
                    context: Some(AgentContext {
                        messages: ctx.context.messages,
                        tools: ctx.context.tools,
                    }),
                    messages: None,
                    model: None,
                    thinking_level: None,
                    reasoning: None,
                }))
            })
        })),
        prepare_request: Some(Arc::new(move |req, _sig| {
            let prep_ref = Arc::clone(&prep_ref);
            Box::pin(async move {
                prep_ref.store(true, Ordering::SeqCst);
                Ok(Some(PrepareRequestResult {
                    context: Some(req.context),
                    model: Some(create_model("prepared-model")),
                    thinking_level: None,
                }))
            })
        })),
        stream_fn: Some(Arc::new(move |model, _ctx, _opts| {
            models_ref.lock().unwrap().push(model.id);
            let n = llm_ref.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![tc_block("call-1", "noop", json!({}))], StopReason::ToolUse),
                        StopReason::ToolUse,
                    ))
                } else {
                    Ok(spawn_done_stream(
                        assistant_msg(vec![text_block("all done")], StopReason::Stop),
                        StopReason::Stop,
                    ))
                }
            })
        })),
        ..Default::default()
    });

    agent.prompt("run hooks").await.expect("prompt succeeds");

    assert!(before_called.load(Ordering::SeqCst));
    assert!(after_called.load(Ordering::SeqCst));
    assert!(next_turn_called.load(Ordering::SeqCst));
    assert!(prepare_req_called.load(Ordering::SeqCst));
    assert_eq!(
        *seen_model_ids.lock().unwrap(),
        vec!["prepared-model", "prepared-model"]
    );
}

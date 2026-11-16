//! Port of packages/agent/test/e2e.test.ts

mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use pi_agent_core::{Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, ThinkingLevel};
use pi_ai::compat::{get_model, register_faux_provider};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, FauxAssistantMessageContent, FauxAssistantMessageOptions,
    FauxContentBlock, FauxModelDefinition, FauxProviderRegistration, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, JsonObject, Message, Model, RegisterFauxProviderOptions, StopReason, ToolResultMessage,
    UserContentBlock, UserMessage, UserMessageContent, faux_assistant_message, faux_text, faux_thinking,
    faux_tool_call,
};
use pi_js::time::now_ms;
use serde_json::{Value, json};
use serial_test::serial;

use support::utils::calculate::calculate_tool;

struct FauxRegistrationGuard(FauxProviderRegistration);

impl Drop for FauxRegistrationGuard {
    fn drop(&mut self) {
        self.0.unregister();
    }
}

fn json_obj(val: Value) -> JsonObject {
    serde_json::from_value(val).expect("valid JsonObject")
}

fn assistant_text(msg: &AssistantMessage) -> String {
    msg.content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn last_assistant_message(messages: &[AgentMessage]) -> &AssistantMessage {
    messages
        .last()
        .and_then(|msg| match msg {
            AgentMessage::Message(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .expect("expected final assistant message")
}

async fn run_basic_prompt(model: Model) {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant. Keep responses concise.".to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    });

    agent
        .prompt("What is 2+2? Answer with just the number.")
        .await
        .expect("prompt should succeed");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
    assert_eq!(state.messages[0].role(), Some("system"));
    assert_eq!(state.messages[1].role(), Some("user"));
    assert_eq!(state.messages[2].role(), Some("assistant"));

    let last = last_assistant_message(&state.messages);
    assert_ne!(last.stop_reason, StopReason::Error);
    assert!(assistant_text(last).contains('4'));
}

async fn run_tool_execution(model: Model) {
    let agent = Arc::new(Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant. Always use the calculate tool for math.".to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(vec![calculate_tool()]),
            ..Default::default()
        }),
        ..Default::default()
    }));

    let saw_pending = Arc::new(Mutex::new(false));
    let saw_pending_ref = Arc::clone(&saw_pending);
    let agent_ref = Arc::clone(&agent);
    let _unsub = agent.subscribe(move |ev| {
        if matches!(ev, AgentEvent::ToolExecutionStart { .. }) && !agent_ref.state().pending_tool_calls.is_empty() {
            *saw_pending_ref.lock().unwrap() = true;
        }
    });

    agent
        .prompt("Calculate 123 * 456 using the calculator tool.")
        .await
        .expect("prompt should succeed");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.pending_tool_calls.is_empty());
    assert!(*saw_pending.lock().unwrap());
    assert!(state.messages.len() >= 5);

    let last = last_assistant_message(&state.messages);
    assert!(assistant_text(last).contains("56088"));
}

async fn run_abort_execution(model: Model) {
    let agent = Arc::new(Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    }));

    let agent_task = Arc::clone(&agent);
    let prompt_handle = tokio::spawn(async move { agent_task.prompt("Count slowly from 1 to 20.").await });

    tokio::time::sleep(Duration::from_millis(30)).await;
    agent.abort();
    prompt_handle.await.unwrap().expect("abort prompt should settle");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 2);

    let last = last_assistant_message(&state.messages);
    assert_eq!(last.stop_reason, StopReason::Aborted);
    assert!(last.error_message.is_some());
    assert_eq!(state.error, last.error_message);
}

async fn run_state_updates(model: Model) {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    });

    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let events_ref = Arc::clone(&events);
    let _unsub = agent.subscribe(move |ev| {
        let tag = match ev {
            AgentEvent::AgentStart => "agent_start",
            AgentEvent::TurnStart => "turn_start",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        };
        events_ref.lock().unwrap().push(tag);
    });

    agent.prompt("Count from 1 to 5.").await.expect("prompt should succeed");

    let captured = events.lock().unwrap();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(captured.contains(&expected), "missing event {expected}");
    }

    let pos = |name: &str| captured.iter().position(|x| *x == name).unwrap();
    let rpos = |name: &str| captured.iter().rposition(|x| *x == name).unwrap();
    assert!(pos("agent_start") < pos("message_start"));
    assert!(pos("message_start") < pos("message_end"));
    assert!(pos("message_end") < rpos("agent_end"));

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
}

async fn run_multi_turn_conversation(model: Model) {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    });

    agent.prompt("My name is Alice.").await.expect("turn 1 should succeed");
    assert_eq!(agent.state().messages.len(), 3);

    agent.prompt("What is my name?").await.expect("turn 2 should succeed");
    let state = agent.state();
    assert_eq!(state.messages.len(), 5);

    let last = last_assistant_message(&state.messages);
    assert!(assistant_text(last).to_lowercase().contains("alice"));
}

#[tokio::test]
#[serial]
async fn faux_handles_basic_text_prompt() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("4".to_string()),
            None,
        ))]);
    run_basic_prompt(faux.0.get_model()).await;
}

#[tokio::test]
#[serial]
async fn faux_executes_tools_and_tracks_pending_tool_calls() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    faux.0.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::Text(faux_text("Let me calculate that.")),
                FauxContentBlock::ToolCall(faux_tool_call(
                    "calculate",
                    json_obj(json!({ "expression": "123 * 456" })),
                    Some(FauxToolCallOptions {
                        id: Some("calc-1".to_string()),
                    }),
                )),
            ]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("The result is 56088.".to_string()),
            None,
        )),
    ]);
    run_tool_execution(faux.0.get_model()).await;
}

#[tokio::test]
#[serial]
async fn faux_handles_abort_during_streaming() {
    let faux = FauxRegistrationGuard(register_faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size: Some(FauxTokenSize {
            min: Some(2),
            max: Some(2),
        }),
        ..Default::default()
    })));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text(
                "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen".to_string(),
            ),
            None,
        ))]);
    run_abort_execution(faux.0.get_model()).await;
}

#[tokio::test]
#[serial]
async fn faux_emits_lifecycle_updates_while_streaming() {
    let faux = FauxRegistrationGuard(register_faux_provider(Some(RegisterFauxProviderOptions {
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    })));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("1 2 3 4 5".to_string()),
            None,
        ))]);
    run_state_updates(faux.0.get_model()).await;
}

#[tokio::test]
#[serial]
async fn faux_maintains_context_across_multiple_turns() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    faux.0.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Nice to meet you, Alice.".to_string()),
            None,
        )),
        FauxResponseStep::Factory(Arc::new(|context, _opts, _state, _model| {
            Box::pin(async move {
                let has_alice = context.messages.iter().any(|message| match message {
                    Message::User(u) => match &u.content {
                        UserMessageContent::Text(s) => s.contains("Alice"),
                        UserMessageContent::Blocks(blocks) => blocks.iter().any(|b| match b {
                            UserContentBlock::Text(t) => t.text.contains("Alice"),
                            _ => false,
                        }),
                    },
                    _ => false,
                });
                let reply = if has_alice {
                    "Your name is Alice."
                } else {
                    "I do not know your name."
                };
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(reply.to_string()),
                    None,
                ))
            })
        })),
    ]);
    run_multi_turn_conversation(faux.0.get_model()).await;
}

#[tokio::test]
#[serial]
async fn faux_preserves_thinking_content_blocks() {
    let faux = FauxRegistrationGuard(register_faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-reasoning".to_string(),
            name: None,
            reasoning: Some(true),
            input: None,
            input_limits: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }]),
        ..Default::default()
    })));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::Thinking(faux_thinking("step by step")),
                FauxContentBlock::Text(faux_text("4")),
            ]),
            None,
        ))]);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(faux.0.get_model()),
            thinking_level: Some(ThinkingLevel::Low),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    });

    agent.prompt("What is 2+2?").await.expect("prompt should succeed");

    let state = agent.state();
    assert_eq!(state.messages.len(), 3);
    let assistant = last_assistant_message(&state.messages);
    assert_eq!(assistant.content.len(), 2);
    match &assistant.content[0] {
        AssistantContentBlock::Thinking(th) => assert_eq!(th.thinking, "step by step"),
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &assistant.content[1] {
        AssistantContentBlock::Text(tx) => assert_eq!(tx.text, "4"),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn faux_continue_throws_when_no_messages_in_context() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Test".to_string()),
            model: Some(faux.0.get_model()),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent.clear_messages();

    let err = agent
        .continue_run()
        .await
        .expect_err("continue on empty context must fail");
    assert!(err.message().contains("No messages to continue from"));
}

#[tokio::test]
#[serial]
async fn faux_continue_throws_when_last_message_is_assistant() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    let model = faux.0.get_model();
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Test".to_string()),
            model: Some(model.clone()),
            ..Default::default()
        }),
        ..Default::default()
    });

    let mut assistant = faux_assistant_message(FauxAssistantMessageContent::Text("Hello".to_string()), None);
    assistant.api = model.api;
    assistant.provider = model.provider;
    assistant.model = model.id;
    agent.replace_messages(vec![AgentMessage::from(assistant)]);

    let err = agent
        .continue_run()
        .await
        .expect_err("continue from assistant must fail");
    assert!(err.message().contains("Cannot continue from message role: assistant"));
}

#[tokio::test]
#[serial]
async fn faux_continue_from_user_message_gets_response() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("HELLO WORLD".to_string()),
            None,
        ))]);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant. Follow instructions exactly.".to_string()),
            model: Some(faux.0.get_model()),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    });

    agent.replace_messages(vec![AgentMessage::from(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(faux_text("Say exactly: HELLO WORLD"))]),
        timestamp: now_ms(),
    })]);

    agent.continue_run().await.expect("continue should succeed");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[0].role(), Some("user"));
    assert_eq!(state.messages[1].role(), Some("assistant"));

    let last = last_assistant_message(&state.messages);
    assert!(assistant_text(last).to_uppercase().contains("HELLO WORLD"));
}

#[tokio::test]
#[serial]
async fn faux_continue_processes_tool_results() {
    let faux = FauxRegistrationGuard(register_faux_provider(None));
    let model = faux.0.get_model();
    faux.0
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("The answer is 8.".to_string()),
            None,
        ))]);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some(
                "You are a helpful assistant. After getting a calculation result, state the answer clearly."
                    .to_string(),
            ),
            model: Some(model.clone()),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(vec![calculate_tool()]),
            ..Default::default()
        }),
        ..Default::default()
    });

    let user_message = AgentMessage::from(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(faux_text("What is 5 + 3?"))]),
        timestamp: now_ms(),
    });

    let mut assistant = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Text(faux_text("Let me calculate that.")),
            FauxContentBlock::ToolCall(faux_tool_call(
                "calculate",
                json_obj(json!({ "expression": "5 + 3" })),
                Some(FauxToolCallOptions {
                    id: Some("calc-1".to_string()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    );
    assistant.api = model.api;
    assistant.provider = model.provider;
    assistant.model = model.id;

    let tool_result: ToolResultMessage = serde_json::from_value(json!({
        "role": "toolResult",
        "toolCallId": "calc-1",
        "toolName": "calculate",
        "content": [{ "type": "text", "text": "5 + 3 = 8" }],
        "isError": false,
        "timestamp": now_ms()
    }))
    .expect("valid toolResult json");

    agent.replace_messages(vec![
        user_message,
        AgentMessage::from(assistant),
        AgentMessage::from(tool_result),
    ]);

    agent.continue_run().await.expect("continue should succeed");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 4);

    let last = last_assistant_message(&state.messages);
    assert!(assistant_text(last).contains('8'));
}

#[tokio::test]
#[ignore = "live e2e requires credentials"]
async fn live_anthropic_e2e_smoke() {
    if std::env::var_os("ANTHROPIC_API_KEY").is_none() {
        return;
    }
    let model = get_model("anthropic", "claude-sonnet-4-5").expect("known anthropic model");
    run_basic_prompt(model.clone()).await;
    run_tool_execution(model.clone()).await;
    run_multi_turn_conversation(model).await;
}

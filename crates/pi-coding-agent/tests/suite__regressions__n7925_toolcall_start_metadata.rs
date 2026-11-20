//! Port of packages/coding-agent/test/suite/regressions/7925-toolcall-start-metadata.test.ts

// #7925

use std::sync::{Arc, Mutex};

use pi_agent_core::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, AgentToolResult, ThinkingLevel,
};
use pi_ai::compat::register_faux_provider;
use pi_ai::{
    AssistantMessageEvent, FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock,
    FauxProviderRegistration, FauxResponseStep, FauxToolCallOptions, JsonObject, Message, StopReason, TextContent,
    UserContentBlock, faux_assistant_message, faux_tool_call,
};
use pi_coding_agent::modes::json_event::{AgentSessionEvent, to_json_event};
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

struct FauxGuard(FauxProviderRegistration);

impl Drop for FauxGuard {
    fn drop(&mut self) {
        self.0.unregister();
    }
}

fn json_obj(val: Value) -> JsonObject {
    serde_json::from_value(val).expect("valid JsonObject")
}

// PORT: `AgentSession` / `tests/support/suite/harness.rs` (Stage 3A) have not merged yet on this branch,
// so streaming `message_update` events are driven directly through `pi_agent_core::Agent` + `register_faux_provider`
// and transformed with `pi_coding_agent::modes::json_event::to_json_event`.
mod regression_7925_tool_call_metadata_is_available_when_streaming_starts {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn includes_the_tool_call_id_and_name_without_cumulative_snapshots() {
        let faux = FauxGuard(register_faux_provider(None));
        faux.0.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                    "write",
                    json_obj(json!({ "path": "output.txt", "content": "x".repeat(100) })),
                    Some(FauxToolCallOptions {
                        id: Some("call_7925".to_string()),
                    }),
                ))),
                Some(FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                }),
            )),
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Text("done".to_string()),
                None,
            )),
        ]);

        let write_tool = AgentTool {
            name: "write".to_string(),
            label: "write".to_string(),
            description: "Write a file".to_string(),
            parameters: t::object([("path", t::string()), ("content", t::string())]).into(),
            constrained_sampling: None,
            output_schema: None,
            execution_mode: None,
            prepare_arguments: None,
            replay: None,
            execute: Arc::new(|_, _, _, _| {
                Box::pin(async {
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                        ..Default::default()
                    })
                })
            }),
        };

        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                model: Some(faux.0.get_model()),
                thinking_level: Some(ThinkingLevel::Off),
                tools: Some(vec![write_tool]),
                ..Default::default()
            }),
            ..Default::default()
        });

        let session_updates = Arc::new(Mutex::new(Vec::<AgentSessionEvent>::new()));
        let updates_ref = Arc::clone(&session_updates);
        let _unsub = agent.subscribe(move |event| {
            if let AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } = event
            {
                updates_ref.lock().unwrap().push(AgentSessionEvent::MessageUpdate {
                    message: message.clone(),
                    assistant_message_event: assistant_message_event.clone(),
                });
            }
        });

        agent.prompt("write a file").await.expect("prompt succeeds");

        let updates = session_updates.lock().unwrap().clone();
        let update = updates
            .into_iter()
            .find(|event| {
                matches!(
                    event,
                    AgentSessionEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ToolCallStart { .. },
                        ..
                    }
                )
            })
            .expect("Expected toolcall_start assistant update");

        let expected_usage = match &update {
            AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Message(Message::Assistant(a)),
                ..
            } => pi_js::json::parse(&pi_js::json::stringify(&a.usage)).expect("valid usage json"),
            _ => unreachable!(),
        };

        let wire_update = to_json_event(&update).expect("to_json_event succeeds");
        let wire_json: Value = pi_js::json::parse(&pi_js::json::stringify(&wire_update)).expect("valid wire json");

        assert_eq!(
            wire_json,
            json!({
                "type": "message_update",
                "usage": expected_usage,
                "assistantMessageEvent": {
                    "type": "toolcall_start",
                    "contentIndex": 0,
                    "id": "call_7925",
                    "toolName": "write"
                }
            })
        );
    }
}

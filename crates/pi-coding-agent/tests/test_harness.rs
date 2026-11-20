//! Port of packages/coding-agent/test/test-harness.test.ts

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::IndexMap;
use pi_agent_core::{AgentEvent, AgentTool, AgentToolResult};
use pi_ai::{AssistantContentBlock, AssistantMessageEvent, JsonObject, StopReason, TextContent};
use pi_coding_agent::core::session_manager::SessionEntry;
use pi_coding_agent::core::settings_manager::{RetrySettings, Settings};
use pi_js::vendor::typebox::t;
use serde_json::Value;

use support::test_harness::{
    FauxResponse, FauxResponseInput, FauxToolCallSpec, FauxUsageOverrides, Harness, HarnessOptions,
    HarnessSessionEvent, create_harness, create_harness_with_extensions,
};
use support::utilities::{CreateTestExtensionsResultInput, TestExtensionInput};

struct CleanupGuard(Option<Harness>);

impl CleanupGuard {
    fn new(h: Harness) -> Self {
        Self(Some(h))
    }

    fn get(&self) -> &Harness {
        self.0.as_ref().expect("harness initialized")
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.cleanup();
        }
    }
}

fn echo_tool(executed: Option<Arc<AtomicBool>>) -> AgentTool {
    let mut map = IndexMap::new();
    map.insert("echo".to_owned(), true);
    let _ = map;
    AgentTool {
        name: "echo".to_owned(),
        label: "Echo".to_owned(),
        description: "Echo back".to_owned(),
        parameters: t::object([("text", t::string())]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _signal, _on_update| {
            let executed = executed.clone();
            Box::pin(async move {
                if let Some(flag) = executed {
                    flag.store(true, Ordering::SeqCst);
                }
                Ok(AgentToolResult::text("echoed"))
            })
        }),
        replay: None,
    }
}

mod test_harness {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn simple_text_response() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("hello world")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        assert_eq!(h.get().faux().call_count, 1);

        let msgs = h.get().session.messages();
        let assistant_messages: Vec<_> = msgs.iter().filter_map(|m| m.as_assistant()).collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(
            assistant_messages[0].content,
            vec![AssistantContentBlock::Text(TextContent::new("hello world"))]
        );
        assert_eq!(assistant_messages[0].stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn response_sequence() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![
                    FauxResponseInput::from("first"),
                    FauxResponseInput::from("second"),
                    FauxResponseInput::from("third"),
                ]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("a").await.unwrap();
        h.get().session.prompt("b").await.unwrap();
        h.get().session.prompt("c").await.unwrap();

        assert_eq!(h.get().faux().call_count, 3);

        let texts: Vec<String> = h
            .get()
            .session
            .messages()
            .iter()
            .filter_map(|m| m.as_assistant())
            .filter_map(|m| {
                m.content.iter().find_map(|b| match b {
                    AssistantContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn tool_call_response_triggers_tool_execution() {
        let executed = Arc::new(AtomicBool::new(false));
        let tool = echo_tool(Some(executed.clone()));
        let mut base_override = IndexMap::new();
        base_override.insert("echo".to_owned(), tool.clone());

        let mut args = JsonObject::new();
        args.insert("text".to_owned(), Value::String("hi".to_owned()));

        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![
                    FauxResponseInput::from(FauxResponse {
                        tool_calls: Some(vec![FauxToolCallSpec {
                            id: None,
                            name: "echo".to_owned(),
                            args,
                        }]),
                        ..Default::default()
                    }),
                    FauxResponseInput::from("done after tool"),
                ]),
                tools: Some(vec![tool]),
                base_tools_override: Some(base_override),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("use the tool").await.unwrap();

        assert!(executed.load(Ordering::SeqCst));
        assert_eq!(h.get().faux().call_count, 2);

        let tool_results: Vec<_> = h
            .get()
            .session
            .messages()
            .into_iter()
            .filter(|m| m.role() == Some("toolResult"))
            .collect();
        assert_eq!(tool_results.len(), 1);
    }

    #[tokio::test]
    async fn error_response() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from(FauxResponse {
                    error: Some("something broke".to_owned()),
                    ..Default::default()
                })]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let msgs = h.get().session.messages();
        let assistant_messages: Vec<_> = msgs.iter().filter_map(|m| m.as_assistant()).collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].stop_reason, StopReason::Error);
        assert_eq!(assistant_messages[0].error_message.as_deref(), Some("something broke"));
    }

    #[tokio::test]
    async fn turns_a_pending_terminal_response_into_an_error() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from(FauxResponse {
                    text: Some("partial".to_owned()),
                    stop_reason: Some(StopReason::Pending),
                    ..Default::default()
                })]),
                settings: Some(Settings {
                    retry: Some(RetrySettings {
                        enabled: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let msgs = h.get().session.messages();
        let assistant_messages: Vec<_> = msgs.iter().filter_map(|m| m.as_assistant()).collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].stop_reason, StopReason::Error);
        assert_eq!(
            assistant_messages[0].error_message.as_deref(),
            Some("Faux response ended without a stop reason")
        );
    }

    #[tokio::test]
    async fn retry_on_transient_error() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![
                    FauxResponseInput::from(FauxResponse {
                        error: Some("overloaded_error".to_owned()),
                        ..Default::default()
                    }),
                    FauxResponseInput::from("recovered"),
                ]),
                settings: Some(Settings {
                    retry: Some(RetrySettings {
                        enabled: Some(true),
                        max_retries: Some(3),
                        base_delay_ms: Some(1),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        assert_eq!(h.get().faux().call_count, 2);

        let snap = h.get().events_snapshot();
        let retry_starts: Vec<_> = snap
            .iter()
            .filter(|e| matches!(e, HarnessSessionEvent::AutoRetryStart { .. }))
            .collect();
        assert_eq!(retry_starts.len(), 1);

        let retry_ends: Vec<_> = snap
            .iter()
            .filter_map(|e| match e {
                HarnessSessionEvent::AutoRetryEnd { success, .. } => Some(*success),
                _ => None,
            })
            .collect();
        assert_eq!(retry_ends, vec![true]);
    }

    #[tokio::test]
    async fn custom_usage_numbers() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from(FauxResponse {
                    text: Some("big response".to_owned()),
                    usage: Some(FauxUsageOverrides {
                        input: Some(100_000),
                        output: Some(5_000),
                        ..Default::default()
                    }),
                    ..Default::default()
                })]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let msgs = h.get().session.messages();
        let msg = msgs.iter().find_map(|m| m.as_assistant()).unwrap();
        assert_eq!(msg.usage.input, 100_000);
        assert_eq!(msg.usage.output, 5_000);
    }

    #[tokio::test]
    async fn event_capture() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("hello")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let events = h.get().agent_events();
        let agent_starts = events.iter().filter(|e| matches!(e, AgentEvent::AgentStart)).count();
        let agent_ends = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AgentEnd { .. }))
            .count();
        let message_ends = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageEnd { .. }))
            .count();

        assert_eq!(agent_starts, 1);
        assert_eq!(agent_ends, 1);
        assert!(message_ends >= 2);
    }

    #[tokio::test]
    async fn context_capture() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("reply")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("my question").await.unwrap();

        let faux = h.get().faux();
        assert_eq!(faux.contexts.len(), 1);
        let ctx = &faux.contexts[0];
        let has_user = ctx.messages.iter().any(|m| m.role() == "user");
        assert!(has_user);
    }

    #[tokio::test]
    async fn wraps_around_when_more_calls_than_responses() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("a"), FauxResponseInput::from("b")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("1").await.unwrap();
        h.get().session.prompt("2").await.unwrap();
        h.get().session.prompt("3").await.unwrap();

        assert_eq!(h.get().faux().call_count, 3);

        let texts: Vec<String> = h
            .get()
            .session
            .messages()
            .iter()
            .filter_map(|m| m.as_assistant())
            .filter_map(|m| {
                m.content.iter().find_map(|b| match b {
                    AssistantContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "a"]);
    }

    #[tokio::test]
    async fn streams_text_deltas() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("hello world")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let deltas: Vec<String> = h
            .get()
            .agent_events()
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::MessageUpdate {
                    assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
                    ..
                } => Some(delta),
                _ => None,
            })
            .collect();
        assert!(!deltas.is_empty());
        assert_eq!(deltas.concat(), "hello world");
    }

    #[tokio::test]
    async fn streams_thinking_deltas() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from(FauxResponse {
                    thinking: Some("let me think about this".to_owned()),
                    text: Some("answer".to_owned()),
                    ..Default::default()
                })]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let events = h.get().agent_events();
        let starts = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ThinkingStart { .. },
                        ..
                    }
                )
            })
            .count();
        let deltas: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageUpdate {
                    assistant_message_event: AssistantMessageEvent::ThinkingDelta { delta, .. },
                    ..
                } => Some(delta.clone()),
                _ => None,
            })
            .collect();
        let ends = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ThinkingEnd { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(starts, 1);
        assert!(!deltas.is_empty());
        assert_eq!(ends, 1);
        assert_eq!(deltas.concat(), "let me think about this");
    }

    #[tokio::test]
    async fn streams_tool_call_deltas() {
        let tool = echo_tool(None);
        let mut base_override = IndexMap::new();
        base_override.insert("echo".to_owned(), tool.clone());
        let mut args = JsonObject::new();
        args.insert("text".to_owned(), Value::String("hi".to_owned()));

        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![
                    FauxResponseInput::from(FauxResponse {
                        tool_calls: Some(vec![FauxToolCallSpec {
                            id: None,
                            name: "echo".to_owned(),
                            args,
                        }]),
                        ..Default::default()
                    }),
                    FauxResponseInput::from("done"),
                ]),
                tools: Some(vec![tool]),
                base_tools_override: Some(base_override),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("use tool").await.unwrap();

        let events = h.get().agent_events();
        let starts = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ToolCallStart { .. },
                        ..
                    }
                )
            })
            .count();
        let deltas = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ToolCallDelta { .. },
                        ..
                    }
                )
            })
            .count();
        let ends = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::MessageUpdate {
                        assistant_message_event: AssistantMessageEvent::ToolCallEnd { .. },
                        ..
                    }
                )
            })
            .count();

        assert_eq!(starts, 1);
        assert!(deltas > 0);
        assert_eq!(ends, 1);
    }

    #[tokio::test]
    async fn streams_thinking_then_text_then_tool_call_in_order() {
        let tool = echo_tool(None);
        let mut base_override = IndexMap::new();
        base_override.insert("echo".to_owned(), tool.clone());
        let mut args = JsonObject::new();
        args.insert("text".to_owned(), Value::String("x".to_owned()));

        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![
                    FauxResponseInput::from(FauxResponse {
                        thinking: Some("hmm".to_owned()),
                        text: Some("I will call a tool".to_owned()),
                        tool_calls: Some(vec![FauxToolCallSpec {
                            id: None,
                            name: "echo".to_owned(),
                            args,
                        }]),
                        ..Default::default()
                    }),
                    FauxResponseInput::from("final"),
                ]),
                tools: Some(vec![tool]),
                base_tools_override: Some(base_override),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("do it").await.unwrap();

        let stream_types: Vec<&'static str> = h
            .get()
            .agent_events()
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => Some(match assistant_message_event {
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
                }),
                _ => None,
            })
            .collect();

        let first_thinking = stream_types.iter().position(|&t| t == "thinking_start").unwrap();
        let first_text = stream_types.iter().position(|&t| t == "text_start").unwrap();
        let first_toolcall = stream_types.iter().position(|&t| t == "toolcall_start").unwrap();

        assert!(first_thinking < first_text);
        assert!(first_text < first_toolcall);
    }

    #[tokio::test]
    async fn loads_inline_extension_factories_and_disambiguates_duplicate_commands() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let calls_a = calls.clone();
        let calls_b = calls.clone();

        let h = CleanupGuard::new(
            create_harness_with_extensions(Some(HarnessOptions {
                extension_factories: Some(vec![
                    TestExtensionInput::Named(CreateTestExtensionsResultInput {
                        path: Some("<alpha>".to_owned()),
                        name: None,
                        factory: Arc::new(move |pi| {
                            let calls_a = calls_a.clone();
                            Box::pin(async move {
                                pi.register_command("shared-cmd", "Alpha command", move |args, _ctx| {
                                    let calls_a = calls_a.clone();
                                    async move {
                                        calls_a.lock().unwrap().push(format!("alpha:{args}"));
                                        Ok(())
                                    }
                                });
                                Ok(())
                            })
                        }),
                    }),
                    TestExtensionInput::Named(CreateTestExtensionsResultInput {
                        path: Some("<beta>".to_owned()),
                        name: None,
                        factory: Arc::new(move |pi| {
                            let calls_b = calls_b.clone();
                            Box::pin(async move {
                                pi.register_command("shared-cmd", "Beta command", move |args, _ctx| {
                                    let calls_b = calls_b.clone();
                                    async move {
                                        calls_b.lock().unwrap().push(format!("beta:{args}"));
                                        Ok(())
                                    }
                                });
                                Ok(())
                            })
                        }),
                    }),
                ]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        let runner = h.get().session.extension_runner.as_ref().unwrap();
        let commands = runner.get_registered_commands();
        let summary: Vec<_> = commands
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.invocation_name.as_str(),
                    c.description.as_str(),
                    c.source_info.path.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("shared-cmd", "shared-cmd:1", "Alpha command", "<alpha>"),
                ("shared-cmd", "shared-cmd:2", "Beta command", "<beta>"),
            ]
        );

        let cmd1 = runner.get_command("shared-cmd:1").unwrap();
        (cmd1.handler)("first".to_owned(), runner.create_command_context())
            .await
            .unwrap();
        let cmd2 = runner.get_command("shared-cmd:2").unwrap();
        (cmd2.handler)("second".to_owned(), runner.create_command_context())
            .await
            .unwrap();

        assert_eq!(*calls.lock().unwrap(), vec!["alpha:first", "beta:second"]);
    }

    #[tokio::test]
    async fn session_persistence_works() {
        let h = CleanupGuard::new(
            create_harness(Some(HarnessOptions {
                responses: Some(vec![FauxResponseInput::from("persisted")]),
                ..Default::default()
            }))
            .await
            .unwrap(),
        );

        h.get().session.prompt("hi").await.unwrap();

        let entries = h.get().session_manager.get_entries();
        let message_entries = entries.iter().filter(|e| matches!(e, SessionEntry::Message(_))).count();
        assert!(message_entries >= 2);
    }
}

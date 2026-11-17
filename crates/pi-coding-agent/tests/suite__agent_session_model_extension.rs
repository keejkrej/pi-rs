//! Port of packages/coding-agent/test/suite/agent-session-model-extension.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    Message, ModelThinkingLevel, StopReason, TextContent, Usage, UsageCost, UserContentBlock, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::transcript::get_current_system_prompt;
use pi_coding_agent::core::agent_session::{ExtensionBindings, ModelMutationOptions};
use pi_coding_agent::core::extensions::runner::ShutdownHandler;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, BeforeAgentStartEventResult, ContextEvent, ContextEventResult, CustomMessageDraft,
    ExtensionApi, InlineExtension, InputEvent, InputEventResult, ModelSelectEvent, ModelSelectSource,
    RegisteredCommandOptions, SessionShutdownEvent, SessionStartEvent, ToolCallEvent, ToolCallEventResult,
    ToolResultContentBlock, ToolResultEvent, ToolResultEventResult,
};
use pi_coding_agent::core::messages::CustomMessageContent;
use pi_coding_agent::core::session_manager::SessionEntry;
use pi_coding_agent::core::settings_manager::Settings;

fn user_content_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|p| match p {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness, get_assistant_texts};

fn faux_def(id: &str, name: Option<&str>, reasoning: Option<bool>) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.to_string(),
        name: name.map(str::to_string),
        reasoning,
        input: None,
        input_limits: None,
        cost: None,
        context_window: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn set_model_saves_the_model_to_the_session_and_emits_model_select() {
    let model_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let me_ref = Arc::clone(&model_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let me = Arc::clone(&me_ref);
        async move {
            pi.on::<ModelSelectEvent>(move |ev, _ctx| {
                let list = Arc::clone(&me);
                Box::pin(async move {
                    let prev = ev.previous_model.as_ref().map(|m| m.id.as_str()).unwrap_or("none");
                    let src = match ev.source {
                        ModelSelectSource::Set => "set",
                        ModelSelectSource::Cycle => "cycle",
                        ModelSelectSource::Restore => "restore",
                    };
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("{prev}->{}:{src}", ev.model.id));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let next_model = harness.get_model_by_id("faux-2").unwrap();
    harness.session.set_model(next_model.clone(), None).await.unwrap();

    assert_eq!(harness.session.model().map(|m| m.id), Some("faux-2".to_string()));
    assert_eq!(
        *model_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["faux-1->faux-2:set".to_string()]
    );
    let model_changes: Vec<String> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::ModelChange(mc) => Some(format!("{}/{}", mc.provider, mc.model_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        model_changes,
        vec![format!("{}/{}", next_model.provider, next_model.id)]
    );
    assert_eq!(harness.settings_manager.get_default_provider(), None);
    assert_eq!(harness.settings_manager.get_default_model(), None);
    harness.cleanup();
}

#[tokio::test]
async fn only_persists_model_and_thinking_defaults_when_requested() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        ..Default::default()
    })
    .await;

    let next_model = harness.get_model_by_id("faux-2").unwrap();
    harness.session.set_model(next_model.clone(), None).await.unwrap();
    assert_eq!(harness.settings_manager.get_default_provider(), None);
    assert_eq!(harness.settings_manager.get_default_model(), None);

    harness.session.set_thinking_level(ModelThinkingLevel::Low, None);
    assert_eq!(harness.settings_manager.get_default_thinking_level(), None);

    harness
        .session
        .set_model(next_model.clone(), Some(ModelMutationOptions { persist: Some(true) }))
        .await
        .unwrap();
    assert_eq!(
        harness.settings_manager.get_default_provider(),
        Some(next_model.provider)
    );
    assert_eq!(harness.settings_manager.get_default_model(), Some(next_model.id));

    harness.session.set_thinking_level(
        ModelThinkingLevel::High,
        Some(ModelMutationOptions { persist: Some(true) }),
    );
    assert_eq!(
        harness.settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::High)
    );
    harness.cleanup();
}

#[tokio::test]
async fn persists_the_requested_default_thinking_level_even_when_the_current_model_clamps_it() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![faux_def("faux-1", None, Some(true))]),
        ..Default::default()
    })
    .await;

    harness.session.set_thinking_level(
        ModelThinkingLevel::Max,
        Some(ModelMutationOptions { persist: Some(true) }),
    );
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::High);
    assert_eq!(
        harness.settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::Max)
    );
    harness.cleanup();
}

#[tokio::test]
async fn cycle_model_and_cycle_thinking_level_are_session_only_by_default() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        settings: Some(Settings {
            default_provider: Some("faux".to_string()),
            default_model: Some("faux-1".to_string()),
            default_thinking_level: Some(ModelThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness.session.cycle_model(None, None).await.unwrap();
    assert_eq!(harness.session.model().map(|m| m.id), Some("faux-2".to_string()));
    assert_eq!(harness.settings_manager.get_default_model(), Some("faux-1".to_string()));

    harness.session.set_thinking_level(ModelThinkingLevel::Off, None);
    assert_eq!(
        harness.session.cycle_thinking_level(None),
        Some(ModelThinkingLevel::Minimal)
    );
    assert_eq!(
        harness.settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::Low)
    );
    harness.cleanup();
}

#[tokio::test]
async fn applies_per_model_thinking_level_override_on_model_switch() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        settings: Some(Settings {
            default_thinking_level: Some(ModelThinkingLevel::Medium),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness
        .settings_manager
        .set_model_thinking_level("faux", "faux-2", ModelThinkingLevel::Low);

    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::High);

    let model2 = harness.get_model_by_id("faux-2").unwrap();
    harness.session.set_model(model2, None).await.unwrap();
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Low);

    let model1 = harness.get_model_by_id("faux-1").unwrap();
    harness.session.set_model(model1, None).await.unwrap();
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Medium);
    harness.cleanup();
}

#[tokio::test]
async fn falls_back_to_current_session_thinking_level_when_no_per_model_or_global_default_is_configured() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        ..Default::default()
    })
    .await;

    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    harness
        .session
        .set_model(harness.get_model_by_id("faux-2").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::High);
    harness.cleanup();
}

#[tokio::test]
async fn per_model_override_takes_priority_over_global_default_during_model_switch() {
    let mut model_thinking_levels = IndexMap::new();
    model_thinking_levels.insert("faux/faux-2".to_string(), ModelThinkingLevel::Minimal);
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        settings: Some(Settings {
            default_thinking_level: Some(ModelThinkingLevel::High),
            model_thinking_levels: Some(model_thinking_levels),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;

    harness
        .session
        .set_model(harness.get_model_by_id("faux-2").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Minimal);
    harness.cleanup();
}

#[tokio::test]
async fn cycles_through_scoped_models_and_preserves_the_scoped_thinking_preference() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(false)),
        ]),
        ..Default::default()
    })
    .await;

    let m1 = harness.get_model_by_id("faux-1").unwrap();
    let m2 = harness.get_model_by_id("faux-2").unwrap();
    harness.session.set_scoped_models(vec![
        pi_coding_agent::core::sdk::ScopedModelEntry {
            model: m1,
            thinking_level: Some(ModelThinkingLevel::High),
        },
        pi_coding_agent::core::sdk::ScopedModelEntry {
            model: m2,
            thinking_level: None,
        },
    ]);
    harness.session.set_thinking_level(ModelThinkingLevel::High, None);

    harness.session.cycle_model(None, None).await.unwrap();
    assert_eq!(harness.session.model().map(|m| m.id), Some("faux-2".to_string()));
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Off);

    harness.session.cycle_model(None, None).await.unwrap();
    assert_eq!(harness.session.model().map(|m| m.id), Some("faux-1".to_string()));
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::High);
    harness.cleanup();
}

#[tokio::test]
async fn clamps_thinking_levels_to_model_capabilities_and_cycles_available_levels() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![faux_def("faux-1", None, Some(false))]),
        ..Default::default()
    })
    .await;

    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    assert_eq!(harness.session.thinking_level(), ModelThinkingLevel::Off);
    assert_eq!(harness.session.cycle_thinking_level(None), None);
    harness.cleanup();
}

#[tokio::test]
async fn cycles_xhigh_before_max_when_both_are_supported() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![faux_def("faux-1", None, Some(true))]),
        ..Default::default()
    })
    .await;

    let mut model = harness.session.model().unwrap();
    let mut map = IndexMap::new();
    map.insert(ModelThinkingLevel::XHigh, Some("xhigh".to_string()));
    map.insert(ModelThinkingLevel::Max, Some("max".to_string()));
    model.thinking_level_map = Some(map);
    harness.session.agent().set_model(model);

    assert_eq!(
        harness.session.get_available_thinking_levels(),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
            ModelThinkingLevel::Max,
        ]
    );
    harness.session.set_thinking_level(ModelThinkingLevel::High, None);
    assert_eq!(
        harness.session.cycle_thinking_level(None),
        Some(ModelThinkingLevel::XHigh)
    );
    assert_eq!(
        harness.session.cycle_thinking_level(None),
        Some(ModelThinkingLevel::Max)
    );
    assert_eq!(
        harness.session.cycle_thinking_level(None),
        Some(ModelThinkingLevel::Off)
    );
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_set_model_is_called_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            faux_def("faux-1", Some("One"), Some(true)),
            faux_def("faux-2", Some("Two"), Some(true)),
        ]),
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;

    let err = harness
        .session
        .set_model(harness.get_model_by_id("faux-2").unwrap(), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("No API key for {}/faux-2", harness.get_model().provider))
    );
    harness.cleanup();
}

#[tokio::test]
async fn allows_extension_tool_call_handlers_to_block_tool_execution() {
    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object([("text", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move { Err(pi_js::Error::msg("tool should have been blocked")) })
        }),
        replay: None,
    };
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<ToolCallEvent>(|_ev, _ctx| {
            Box::pin(async move {
                Ok(Some(ToolCallEventResult {
                    block: Some(true),
                    reason: Some("Blocked by test".to_string()),
                    terminate: None,
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let mut args = IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(|ctx, _opts, _st, _mdl| {
            Box::pin(async move {
                let error_text = ctx
                    .messages
                    .iter()
                    .find_map(|m| match m {
                        Message::ToolResult(tr) => Some(
                            tr.content
                                .iter()
                                .filter_map(|p| match p {
                                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default();
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(error_text),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("hi", None).await.unwrap();
    assert!(get_assistant_texts(&harness).contains(&"Blocked by test".to_string()));
    assert!(harness.session.messages().into_iter().any(|m| match m {
        AgentMessage::Message(Message::ToolResult(tr)) => tr.is_error,
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn allows_extension_tool_result_handlers_to_modify_tool_results() {
    let tool_usage = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
    };
    let patched_usage = Usage {
        input: 5,
        output: 6,
        cache_read: 7,
        cache_write: 8,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 26,
        cost: UsageCost {
            input: 0.5,
            output: 0.6,
            cache_read: 0.7,
            cache_write: 0.8,
            total: 2.6,
        },
    };
    let observed_usage: Arc<Mutex<Option<Usage>>> = Arc::new(Mutex::new(None));
    let u_for_tool = tool_usage;
    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object([("text", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, params, _sig, _upd| {
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(text))],
                    details: Some(json!({ "text": text })),
                    usage: Some(u_for_tool),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let obs_ref = Arc::clone(&observed_usage);
    let patched_for_ext = patched_usage;
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let obs = Arc::clone(&obs_ref);
        async move {
            pi.on::<ToolResultEvent>(move |ev, _ctx| {
                let o = Arc::clone(&obs);
                let patched = patched_for_ext;
                Box::pin(async move {
                    if let ToolResultEvent::Custom(c) = ev {
                        *o.lock().unwrap_or_else(PoisonError::into_inner) = c.usage;
                    }
                    Ok(Some(ToolResultEventResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new("patched result"))]),
                        details: Some(json!({ "patched": true })),
                        structured_content: None,
                        is_error: None,
                        usage: Some(patched),
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let mut args = IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(|ctx, _opts, _st, _mdl| {
            Box::pin(async move {
                let text = ctx
                    .messages
                    .iter()
                    .find_map(|m| match m {
                        Message::ToolResult(tr) => Some(
                            tr.content
                                .iter()
                                .filter_map(|p| match p {
                                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default();
                Ok(faux_assistant_message(FauxAssistantMessageContent::Text(text), None))
            })
        })),
    ]);

    harness.session.prompt("hi", None).await.unwrap();
    assert!(get_assistant_texts(&harness).contains(&"patched result".to_string()));
    assert_eq!(
        *observed_usage.lock().unwrap_or_else(PoisonError::into_inner),
        Some(tool_usage)
    );
    let tr = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            AgentMessage::Message(Message::ToolResult(tr))
                if tr
                    .details
                    .as_ref()
                    .and_then(|d| d.get("patched"))
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                Some(tr)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(tr.usage, Some(patched_usage));
    harness.cleanup();
}

#[tokio::test]
async fn allows_extension_context_handlers_to_modify_messages_before_the_llm_call() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<ContextEvent>(|ev, _ctx| {
            Box::pin(async move {
                let msgs = ev
                    .messages
                    .into_iter()
                    .map(|m| match m {
                        AgentMessage::Message(Message::User(mut u)) => {
                            u.content =
                                UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("rewritten"))]);
                            AgentMessage::Message(Message::User(u))
                        }
                        other => other,
                    })
                    .collect();
                Ok(Some(ContextEventResult { messages: Some(msgs) }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let provider_user_text = Arc::new(Mutex::new(String::new()));
    let put_ref = Arc::clone(&provider_user_text);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let put = Arc::clone(&put_ref);
            Box::pin(async move {
                let text = ctx
                    .messages
                    .iter()
                    .find_map(|m| match m {
                        Message::User(u) => Some(user_content_text(&u.content)),
                        _ => None,
                    })
                    .unwrap_or_default();
                *put.lock().unwrap_or_else(PoisonError::into_inner) = text;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("original", None).await.unwrap();
    assert_eq!(
        *provider_user_text.lock().unwrap_or_else(PoisonError::into_inner),
        "rewritten"
    );
    let stored = harness
        .session
        .messages()
        .into_iter()
        .find_map(|m| match m {
            AgentMessage::Message(Message::User(u)) => Some(u),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        stored.content,
        UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("original"))])
    );
    harness.cleanup();
}

#[tokio::test]
async fn allows_extension_input_handlers_to_transform_or_handle_input() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<InputEvent>(|ev, _ctx| {
            Box::pin(async move {
                if ev.text == "ping" {
                    return Ok(Some(InputEventResult::Handled));
                }
                Ok(Some(InputEventResult::Transform {
                    text: format!("transformed:{}", ev.text),
                    images: None,
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let provider_user_text = Arc::new(Mutex::new(String::new()));
    let put_ref = Arc::clone(&provider_user_text);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let put = Arc::clone(&put_ref);
            Box::pin(async move {
                let text = ctx
                    .messages
                    .iter()
                    .find_map(|m| match m {
                        Message::User(u) => Some(user_content_text(&u.content)),
                        _ => None,
                    })
                    .unwrap_or_default();
                *put.lock().unwrap_or_else(PoisonError::into_inner) = text;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("hello", None).await.unwrap();
    harness.session.prompt("ping", None).await.unwrap();

    assert_eq!(
        *provider_user_text.lock().unwrap_or_else(PoisonError::into_inner),
        "transformed:hello"
    );
    let user_count = harness
        .session
        .messages()
        .into_iter()
        .filter(|m| m.role() == Some("user"))
        .count();
    assert_eq!(user_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn allows_extension_commands_to_inspect_live_system_prompt_options() {
    let seen_cwds: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_tools: Arc<Mutex<Vec<Option<Vec<String>>>>> = Arc::new(Mutex::new(Vec::new()));
    let cwds_ref = Arc::clone(&seen_cwds);
    let tools_ref = Arc::clone(&seen_tools);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cwds = Arc::clone(&cwds_ref);
        let tools = Arc::clone(&tools_ref);
        async move {
            pi.register_command(
                "inspect-options",
                RegisteredCommandOptions {
                    description: Some("Inspect system prompt options".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, ctx| {
                        let cwds_inner = Arc::clone(&cwds);
                        let tools_inner = Arc::clone(&tools);
                        Box::pin(async move {
                            let options = ctx.get_system_prompt_options()?;
                            cwds_inner
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(Some(options.cwd));
                            tools_inner
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(options.selected_tools);
                            Ok(())
                        })
                    }),
                },
            )?;
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.session.prompt("/inspect-options", None).await.unwrap();
    harness.session.prompt("/inspect-options", None).await.unwrap();

    let cwds = seen_cwds.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let tools = seen_tools.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(cwds.len(), 2);
    assert_eq!(cwds[0].as_deref(), Some(harness.temp_dir.as_str()));
    assert!(tools[0].as_ref().is_some_and(|t| t.iter().any(|n| n == "read")));
    harness.cleanup();
}

#[tokio::test]
async fn allows_before_agent_start_handlers_to_inject_custom_messages_and_modify_the_system_prompt() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<BeforeAgentStartEvent>(|ev, _ctx| {
            Box::pin(async move {
                Ok(Some(BeforeAgentStartEventResult {
                    message: Some(CustomMessageDraft {
                        custom_type: "before-start".to_string(),
                        content: CustomMessageContent::Text("injected".to_string()),
                        display: true,
                        details: Some(json!({ "injected": true })),
                    }),
                    system_prompt: Some(format!("{}\n\nextra instructions", ev.system_prompt)),
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    let provider_sys = Arc::new(Mutex::new(String::new()));
    let saw_injected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ps_ref = Arc::clone(&provider_sys);
    let si_ref = Arc::clone(&saw_injected);

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let ps = Arc::clone(&ps_ref);
            let si = Arc::clone(&si_ref);
            Box::pin(async move {
                *ps.lock().unwrap_or_else(PoisonError::into_inner) = get_current_system_prompt(&ctx.messages);
                let found = ctx.messages.iter().any(|m| match m {
                    Message::User(u) => match &u.content {
                        UserMessageContent::Text(s) => s == "injected",
                        UserMessageContent::Blocks(blocks) => blocks.iter().any(|p| match p {
                            UserContentBlock::Text(t) => t.text == "injected",
                            _ => false,
                        }),
                    },
                    _ => false,
                });
                si.store(found, std::sync::atomic::Ordering::SeqCst);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("done".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("hello", None).await.unwrap();
    assert!(
        provider_sys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains("extra instructions")
    );
    assert!(saw_injected.load(std::sync::atomic::Ordering::SeqCst));
    assert!(harness.session.messages().into_iter().any(|m| match m {
        AgentMessage::Custom(v) => {
            v.get("role").and_then(Value::as_str) == Some("custom")
                && v.get("customType").and_then(Value::as_str) == Some("before-start")
        }
        _ => false,
    }));
    harness.cleanup();
}

#[tokio::test]
async fn bind_extensions_emits_session_start_and_reload_emits_session_shutdown_then_session_start() {
    let lifecycle_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let le_start = Arc::clone(&lifecycle_events);
    let le_shut = Arc::clone(&lifecycle_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let l1 = Arc::clone(&le_start);
        let l2 = Arc::clone(&le_shut);
        async move {
            pi.on::<SessionStartEvent>(move |ev, _ctx| {
                let list = Arc::clone(&l1);
                Box::pin(async move {
                    let r = match ev.reason {
                        pi_coding_agent::core::extensions::types::SessionStartReason::Startup => "startup",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Reload => "reload",
                        pi_coding_agent::core::extensions::types::SessionStartReason::New => "new",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Resume => "resume",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Fork => "fork",
                    };
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("start:{r}"));
                    Ok(None)
                })
            });
            pi.on::<SessionShutdownEvent>(move |ev, _ctx| {
                let list = Arc::clone(&l2);
                Box::pin(async move {
                    let r = match ev.reason {
                        pi_coding_agent::core::extensions::types::SessionShutdownReason::Quit => "quit",
                        pi_coding_agent::core::extensions::types::SessionShutdownReason::Reload => "reload",
                        pi_coding_agent::core::extensions::types::SessionShutdownReason::New => "new",
                        pi_coding_agent::core::extensions::types::SessionShutdownReason::Resume => "resume",
                        pi_coding_agent::core::extensions::types::SessionShutdownReason::Fork => "fork",
                    };
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("shutdown:{r}"));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let shutdown_cb: ShutdownHandler = Arc::new(|| {});
    harness
        .session
        .bind_extensions(ExtensionBindings {
            shutdown_handler: Some(shutdown_cb),
            ..Default::default()
        })
        .await
        .unwrap();
    harness.session.reload(None).await.unwrap();

    assert_eq!(
        *lifecycle_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:startup", "shutdown:reload", "start:reload"]
    );
    harness.cleanup();
}

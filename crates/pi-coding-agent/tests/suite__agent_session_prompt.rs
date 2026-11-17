//! Port of packages/coding-agent/test/suite/agent-session-prompt.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{
    ImageContent, Message, ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, StopReason, TextContent,
    UserContentBlock, UserMessageContent,
};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, ExtensionBindings, PromptOptions, SendUserMessageOptions, StreamingBehavior,
};
use pi_coding_agent::core::compaction::CompactionResult;
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEvent, ExtensionApi, InlineExtension, InputDeliveryBehavior, InputEvent, RegisteredCommandOptions,
    SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::messages::CustomMessageContent;
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, PromptsResult, ResourceLoader, SkillsResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pi_coding_agent::core::skills::Skill;
use pi_coding_agent::core::source_info::{
    SourceOrigin, SourceScope, SyntheticSourceOptions, create_synthetic_source_info,
};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{HarnessOptions, create_harness, get_message_text};

const PNG_4X4_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR4nGP4z8AARwzEcQCukw/x0F8jngAAAABJRU5ErkJggg==";

#[tokio::test]
async fn prompts_while_idle_and_records_a_single_text_response() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_string()),
        None,
    ))]);

    harness.session.prompt("hi", None).await.unwrap();

    let roles: Vec<&str> = harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| match m.role() {
            Some("system") => Some("system"),
            Some("user") => Some("user"),
            Some("assistant") => Some("assistant"),
            Some(_) => Some("other"),
            None => None,
        })
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant"]);
    assert_eq!(get_message_text(&harness.session.messages()[1]), "hi");
    assert_eq!(harness.get_pending_response_count(), 0);
    harness.cleanup();
}

#[tokio::test]
async fn handles_a_tool_call_turn_and_waits_for_the_follow_up_llm_response() {
    let tool_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let tr_ref = Arc::clone(&tool_runs);
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
            let runs = Arc::clone(&tr_ref);
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                runs.lock().unwrap_or_else(PoisonError::into_inner).push(text.clone());
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("echo:{text}")))],
                    details: Some(json!({ "text": text })),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        ..Default::default()
    })
    .await;
    let mut args = indexmap::IndexMap::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
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

    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(
        *tool_runs.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello".to_string()]
    );
    let msgs = harness.session.messages();
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[3].role(), Some("toolResult"));
    assert_eq!(msgs[4].role(), Some("assistant"));
    harness.cleanup();
}

#[tokio::test]
async fn executes_multiple_tool_calls_from_one_response_and_continues_with_a_single_follow_up_response() {
    let tool_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let make_tool = |name: &'static str, delay_ms: u64| {
        let runs = Arc::clone(&tool_runs);
        AgentTool {
            name: name.to_string(),
            label: name.to_string(),
            description: format!("{name} tool"),
            parameters: t::object([("value", t::string())]),
            constrained_sampling: None,
            output_schema: None,
            execution_mode: None,
            prepare_arguments: None,
            execute: Arc::new(move |_id, params, _sig, _upd| {
                let runs_inner = Arc::clone(&runs);
                Box::pin(async move {
                    let val = params.get("value").and_then(Value::as_str).unwrap_or("").to_string();
                    if delay_ms > 0 {
                        let _ = pi_js::time::sleep(delay_ms, None).await;
                    }
                    runs_inner
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("{name}:{val}"));
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new(format!("{name}:{val}")))],
                        details: Some(json!({ "value": val })),
                        ..Default::default()
                    })
                })
            }),
            replay: None,
        }
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![make_tool("slow", 25), make_tool("fast", 0)]),
        ..Default::default()
    })
    .await;

    let mut a1 = indexmap::IndexMap::new();
    a1.insert("value".to_string(), Value::String("a".to_string()));
    let mut a2 = indexmap::IndexMap::new();
    a2.insert("value".to_string(), Value::String("b".to_string()));

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::ToolCall(faux_tool_call("slow", a1, None)),
                FauxContentBlock::ToolCall(faux_tool_call("fast", a2, None)),
            ]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(|ctx, _opts, _st, _mdl| {
            Box::pin(async move {
                let count = ctx
                    .messages
                    .iter()
                    .filter(|m| matches!(m, Message::ToolResult(_)))
                    .count();
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(format!("tool results: {count}")),
                    None,
                ))
            })
        })),
    ]);

    harness.session.prompt("run tools", None).await.unwrap();

    let mut recorded = tool_runs.lock().unwrap_or_else(PoisonError::into_inner).clone();
    recorded.sort();
    assert_eq!(recorded, vec!["fast:b".to_string(), "slow:a".to_string()]);
    assert_eq!(
        harness
            .session
            .messages()
            .into_iter()
            .filter(|m| m.role() == Some("toolResult"))
            .count(),
        2
    );
    assert_eq!(
        harness.session.messages().last().and_then(|m| m.role()),
        Some("assistant")
    );
    harness.cleanup();
}

#[tokio::test]
async fn preserves_image_attachments_in_the_provider_context() {
    let harness = create_harness(HarnessOptions::default()).await;
    let saw_image = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let si_ref = Arc::clone(&saw_image);

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let si = Arc::clone(&si_ref);
            Box::pin(async move {
                let found = ctx.messages.iter().any(|m| match m {
                    Message::User(u) => match &u.content {
                        UserMessageContent::Blocks(blocks) => {
                            blocks.iter().any(|p| matches!(p, UserContentBlock::Image(_)))
                        }
                        UserMessageContent::Text(_) => false,
                    },
                    _ => false,
                });
                si.store(found, std::sync::atomic::Ordering::SeqCst);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness
        .session
        .prompt(
            "describe",
            Some(PromptOptions {
                images: Some(vec![ImageContent {
                    data: PNG_4X4_BASE64.to_string(),
                    mime_type: "image/png".to_string(),
                }]),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert!(saw_image.load(std::sync::atomic::Ordering::SeqCst));
    harness.cleanup();
}

#[tokio::test]
async fn uses_the_model_selected_by_before_agent_start_for_image_normalization() {
    let strict_slot: Arc<Mutex<Option<pi_ai::types::Model>>> = Arc::new(Mutex::new(None));
    let ss_ref = Arc::clone(&strict_slot);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let slot = Arc::clone(&ss_ref);
        async move {
            let pi_ev = pi.clone();
            pi.on::<BeforeAgentStartEvent>(move |_ev, _ctx| {
                let s = Arc::clone(&slot);
                let pi_handle = pi_ev.clone();
                Box::pin(async move {
                    let strict_model = s.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
                    let _ = pi_handle.set_model(strict_model).await?;
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "wide".to_string(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
            FauxModelDefinition {
                id: "strict".to_string(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
        ]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let mut strict_model = harness.get_model_by_id("strict").unwrap();
    strict_model.input_limits = Some(ModelInputLimits {
        max_request_bytes: None,
        images: Some(ModelImageInputLimits {
            resize: Some(ModelImageResizeOptions {
                max_width: Some(1),
                max_height: Some(1),
                max_bytes: Some(500_000),
                jpeg_quality: Some(70.0),
            }),
            max_per_message: None,
            max_per_request: None,
        }),
    });
    *strict_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(strict_model);

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("done".to_string()),
        None,
    ))]);

    harness
        .session
        .prompt(
            "inspect",
            Some(PromptOptions {
                images: Some(vec![ImageContent {
                    data: PNG_4X4_BASE64.to_string(),
                    mime_type: "image/png".to_string(),
                }]),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert_eq!(harness.session.model().map(|m| m.id), Some("strict".to_string()));
    let user_msg = harness
        .session
        .messages()
        .into_iter()
        .find(|m| m.role() == Some("user"))
        .unwrap();
    assert!(get_message_text(&user_msg).contains("original 4x4, displayed at 1x1"));
    harness.cleanup();
}

#[tokio::test]
async fn expands_skill_commands_before_sending_the_prompt() {
    let skill_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-skill-"])).unwrap();
    let skill_path = pi_js::path::join(&[&skill_dir, "test-skill.md"]);
    pi_js::fs::write_file(&skill_path, b"# Test Skill\n\nUse the skill body.", None).unwrap();

    let sp_clone = skill_path.clone();
    let sd_clone = skill_dir.clone();
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        skills_override: Some(Arc::new(move |_| SkillsResult {
            skills: vec![Skill {
                name: "test".to_string(),
                description: "Test skill".to_string(),
                file_path: sp_clone.clone(),
                base_dir: sd_clone.clone(),
                source_info: create_synthetic_source_info(
                    &sp_clone,
                    SyntheticSourceOptions {
                        source: "local".to_string(),
                        scope: Some(SourceScope::Project),
                        origin: Some(SourceOrigin::TopLevel),
                        base_dir: Some(sd_clone.clone()),
                        changelog_path: None,
                    },
                ),
                disable_model_invocation: false,
            }],
            diagnostics: Vec::new(),
        })),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let harness = create_harness(HarnessOptions {
        resource_loader: Some(resource_loader),
        ..Default::default()
    })
    .await;

    let expanded = Arc::new(Mutex::new(String::new()));
    let ex_ref = Arc::clone(&expanded);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let ex = Arc::clone(&ex_ref);
            Box::pin(async move {
                if let Some(Message::User(u)) = ctx.messages.iter().find(|m| matches!(m, Message::User(_))) {
                    let txt = match &u.content {
                        UserMessageContent::Text(s) => s.clone(),
                        UserMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|p| match p {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    *ex.lock().unwrap_or_else(PoisonError::into_inner) = txt;
                }
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("/skill:test explain this", None).await.unwrap();
    let captured = expanded.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(captured.contains(r#"<skill name="test" location=""#));
    assert!(captured.contains("Use the skill body."));
    assert!(captured.contains("explain this"));

    harness.cleanup();
    let _ = pi_js::fs::rm(&skill_dir, true, true);
}

#[tokio::test]
async fn expands_prompt_templates_before_sending_the_prompt() {
    let template = PromptTemplate {
        name: "review".to_string(),
        description: "Review template".to_string(),
        argument_hint: None,
        content: "Review this code: $1".to_string(),
        file_path: "/virtual/review.md".to_string(),
        source_info: create_synthetic_source_info(
            "/virtual/review.md",
            SyntheticSourceOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::Temporary),
                origin: Some(SourceOrigin::TopLevel),
                base_dir: None,
                changelog_path: None,
            },
        ),
    };
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        prompts_override: Some(Arc::new(move |_| PromptsResult {
            prompts: vec![template.clone()],
            diagnostics: Vec::new(),
        })),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let harness = create_harness(HarnessOptions {
        resource_loader: Some(resource_loader),
        ..Default::default()
    })
    .await;

    let expanded = Arc::new(Mutex::new(String::new()));
    let ex_ref = Arc::clone(&expanded);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let ex = Arc::clone(&ex_ref);
            Box::pin(async move {
                if let Some(Message::User(u)) = ctx.messages.iter().find(|m| matches!(m, Message::User(_))) {
                    let txt = match &u.content {
                        UserMessageContent::Text(s) => s.clone(),
                        UserMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|p| match p {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    *ex.lock().unwrap_or_else(PoisonError::into_inner) = txt;
                }
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness.session.prompt("/review src/index.ts", None).await.unwrap();
    assert_eq!(
        *expanded.lock().unwrap_or_else(PoisonError::into_inner),
        "Review this code: src/index.ts"
    );
    harness.cleanup();
}

#[tokio::test]
async fn send_user_message_can_opt_into_prompt_template_expansion() {
    let template = PromptTemplate {
        name: "review".to_string(),
        description: "Review template".to_string(),
        argument_hint: None,
        content: "Review this code: $1".to_string(),
        file_path: "/virtual/review.md".to_string(),
        source_info: create_synthetic_source_info(
            "/virtual/review.md",
            SyntheticSourceOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::Temporary),
                origin: Some(SourceOrigin::TopLevel),
                base_dir: None,
                changelog_path: None,
            },
        ),
    };
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        prompts_override: Some(Arc::new(move |_| PromptsResult {
            prompts: vec![template.clone()],
            diagnostics: Vec::new(),
        })),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let harness = create_harness(HarnessOptions {
        resource_loader: Some(resource_loader),
        ..Default::default()
    })
    .await;

    let expanded = Arc::new(Mutex::new(String::new()));
    let ex_ref = Arc::clone(&expanded);
    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(
        move |ctx, _opts, _st, _mdl| {
            let ex = Arc::clone(&ex_ref);
            Box::pin(async move {
                if let Some(Message::User(u)) = ctx.messages.iter().find(|m| matches!(m, Message::User(_))) {
                    let txt = match &u.content {
                        UserMessageContent::Text(s) => s.clone(),
                        UserMessageContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|p| match p {
                                UserContentBlock::Text(t) => Some(t.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    *ex.lock().unwrap_or_else(PoisonError::into_inner) = txt;
                }
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text("ok".to_string()),
                    None,
                ))
            })
        },
    ))]);

    harness
        .session
        .send_user_message(
            "/review src/index.ts",
            Some(SendUserMessageOptions {
                deliver_as: None,
                expand_prompt_templates: Some(true),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        *expanded.lock().unwrap_or_else(PoisonError::into_inner),
        "Review this code: src/index.ts"
    );
    harness.cleanup();
}

#[tokio::test]
async fn dispatches_extension_commands_without_consuming_a_provider_response() {
    let command_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cr_ref = Arc::clone(&command_runs);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cr = Arc::clone(&cr_ref);
        async move {
            pi.register_command(
                "testcmd",
                RegisteredCommandOptions {
                    description: Some("Test command".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, _ctx| {
                        let cr_inner = Arc::clone(&cr);
                        Box::pin(async move {
                            cr_inner.lock().unwrap_or_else(PoisonError::into_inner).push(args);
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
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("should stay queued".to_string()),
        None,
    ))]);

    harness.session.prompt("/testcmd hello world", None).await.unwrap();
    assert_eq!(
        *command_runs.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello world".to_string()]
    );
    assert!(harness.session.messages().is_empty());
    assert_eq!(harness.get_pending_response_count(), 1);
    harness.cleanup();
}

#[tokio::test]
async fn extension_send_user_message_can_opt_into_extension_command_dispatch() {
    let ext_api: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));
    let (cmd_tx, cmd_rx) = tokio::sync::oneshot::channel::<String>();
    let cmd_slot = Arc::new(Mutex::new(Some(cmd_tx)));
    let ea_ref = Arc::clone(&ext_api);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let slot = Arc::clone(&ea_ref);
        let cs = Arc::clone(&cmd_slot);
        async move {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi.clone());
            pi.register_command(
                "testcmd",
                RegisteredCommandOptions {
                    description: Some("Test command".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, _ctx| {
                        let cs_inner = Arc::clone(&cs);
                        Box::pin(async move {
                            if let Some(tx) = cs_inner.lock().unwrap_or_else(PoisonError::into_inner).take() {
                                let _ = tx.send(args);
                            }
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
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();

    let api = ext_api.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap();
    api.send_user_message(
        CustomMessageContent::Text("/testcmd hello world".to_string()),
        Some(pi_coding_agent::core::extensions::types::SendUserMessageOptions {
            deliver_as: None,
            expand_prompt_templates: Some(true),
        }),
    )
    .unwrap();

    assert_eq!(cmd_rx.await.unwrap(), "hello world");
    assert!(harness.session.messages().is_empty());
    assert_eq!(harness.get_pending_response_count(), 0);
    harness.cleanup();
}

#[tokio::test]
async fn send_user_message_while_idle_triggers_a_turn() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("response".to_string()),
        None,
    ))]);

    harness.session.send_user_message("from extension", None).await.unwrap();

    let msgs = harness.session.messages();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role(), Some("system"));
    assert_eq!(msgs[1].role(), Some("user"));
    assert_eq!(msgs[2].role(), Some("assistant"));
    assert_eq!(get_message_text(&msgs[1]), "from extension");
    harness.cleanup();
}

#[tokio::test]
async fn does_not_report_streaming_behavior_to_input_handlers_while_idle() {
    let input_events: Arc<Mutex<Vec<Option<InputDeliveryBehavior>>>> = Arc::new(Mutex::new(Vec::new()));
    let ie_ref = Arc::clone(&input_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ie = Arc::clone(&ie_ref);
        async move {
            pi.on::<InputEvent>(move |ev, _ctx| {
                let list = Arc::clone(&ie);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(ev.streaming_behavior);
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
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("ok".to_string()),
        None,
    ))]);

    harness
        .session
        .prompt(
            "idle",
            Some(PromptOptions {
                streaming_behavior: Some(StreamingBehavior::FollowUp),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert_eq!(*input_events.lock().unwrap_or_else(PoisonError::into_inner), vec![None]);
    harness.cleanup();
}

#[tokio::test]
async fn reports_streaming_behavior_to_input_handlers_while_streaming() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for release".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rx_slot = Arc::clone(&release_rx);
            Box::pin(async move {
                if let Some(rx) = rx_slot.lock().await.take() {
                    let _ = rx.await;
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let input_events: Arc<Mutex<Vec<Option<InputDeliveryBehavior>>>> = Arc::new(Mutex::new(Vec::new()));
    let ie_ref = Arc::clone(&input_events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ie = Arc::clone(&ie_ref);
        async move {
            pi.on::<InputEvent>(move |ev, _ctx| {
                let list = Arc::clone(&ie);
                Box::pin(async move {
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(ev.streaming_behavior);
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                Default::default(),
                None,
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

    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let start_slot = Arc::new(Mutex::new(Some(start_tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::ToolExecutionStart { .. })
            && let Some(sender) = start_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = sender.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_task = tokio::spawn(async move { sess.prompt("start", None).await });
    let _ = start_rx.await;

    harness
        .session
        .prompt(
            "queued",
            Some(PromptOptions {
                streaming_behavior: Some(StreamingBehavior::FollowUp),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        *input_events.lock().unwrap_or_else(PoisonError::into_inner),
        vec![None, Some(InputDeliveryBehavior::FollowUp)]
    );

    let _ = release_tx.send(());
    prompt_task.await.unwrap().unwrap();
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_prompted_during_streaming_without_a_streaming_behavior() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for release".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rx_slot = Arc::clone(&release_rx);
            Box::pin(async move {
                if let Some(rx) = rx_slot.lock().await.take() {
                    let _ = rx.await;
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                Default::default(),
                None,
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

    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let start_slot = Arc::new(Mutex::new(Some(start_tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::ToolExecutionStart { .. })
            && let Some(sender) = start_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = sender.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_task = tokio::spawn(async move { sess.prompt("start", None).await });
    let _ = start_rx.await;

    let err = harness.session.prompt("second", None).await.unwrap_err();
    assert!(err.to_string().contains(
        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
    ));

    let _ = release_tx.send(());
    prompt_task.await.unwrap().unwrap();
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_prompted_during_manual_compaction() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_slot = Arc::new(Mutex::new(Some(started_tx)));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ss = Arc::clone(&started_slot);
        let rr = Arc::clone(&release_rx);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |ev, _ctx| {
                let s = Arc::clone(&ss);
                let r = Arc::clone(&rr);
                Box::pin(async move {
                    if let Some(tx) = s.lock().unwrap_or_else(PoisonError::into_inner).take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = r.lock().await.take() {
                        let _ = rx.await;
                    }
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: None,
                        compaction: Some(CompactionResult {
                            summary: "manual compacted".to_string(),
                            first_kept_entry_id: ev.preparation.first_kept_entry_id,
                            tokens_before: ev.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({})),
                        }),
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .unwrap();
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("one".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("two".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("first", None).await.unwrap();
    harness.session.prompt("second", None).await.unwrap();

    let sess = harness.session.clone();
    let compact_task = tokio::spawn(async move { sess.compact(None).await });
    let _ = started_rx.await;

    let err = harness.session.prompt("third", None).await.unwrap_err();
    assert!(
        err.to_string().contains(
            "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
        )
    );

    let _ = release_tx.send(());
    let _ = compact_task.await;
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_prompting_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await;
    let err = harness.session.prompt("hi", None).await.unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("No API key found for {}.", harness.get_model().provider))
    );
    harness.cleanup();
}

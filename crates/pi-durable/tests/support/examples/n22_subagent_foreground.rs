//! Port of packages/durable/test/examples/22-subagent-foreground.ts
#![allow(dead_code)]

// A foreground subagent tool: the parent's tool call creates a child conversation it owns, runs one task there, and
// returns the child's answer. Aborting the tool call aborts the child. A UI finds the child through the tool's running
// details and shows its events under the call.
// PORT: Offline adaptation omitting OPENAI_API_KEY; uses the scripted faux model.
// Run via `spec_usage::examples_smoke`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::types::{AssistantContentBlock, Message, StopReason, TextContent};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    Type, create_models, faux_assistant_message, faux_provider, faux_text, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::events::{AgentEvent, AgentEventStream};
use pi_durable::harness::types::{ToolExecutionApi, ToolExecutionResult, ToolResultContentBlock};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, EntryId, InputSubmissionState, JsonValue, SubmissionState,
};
use pi_durable::{
    AGENT_DOC, ASSISTANT_ENTRY, AgentChange, AgentExtensionsChange, Extension, FieldChange, Harness, HarnessOptions,
    InputSubmissionDraft, MemoryStorage, ModelRef, Registry, RegistryReader, RootConversationOptions, SubmissionDraft,
    ToolRegistration, ToolReplayMode, UserInput, configure, create_registry, define_extension, define_tool,
    watch_events,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentDetails {
    conversation_id: ConversationId,
}

async fn answer_text(api: &ToolExecutionApi, answer: EntryId, call_context: Context) -> pi_js::Result<String> {
    let entry = api
        .commit(move |tx| async move { tx.entry(answer).await }, call_context)
        .await?;
    let Some(Message::Assistant(msg)) = entry.and_then(|e| e.model).and_then(|mut m| m.drain(..).next()) else {
        return Ok(String::new());
    };
    Ok(msg
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect())
}

fn subagent_extension() -> Extension {
    define_extension(Extension {
        name: "subagent".to_owned(),
        tools: vec![define_tool(ToolRegistration {
            name: "subagent".to_owned(),
            description: "Delegate a self-contained task to a subagent and get its answer back.".to_owned(),
            parameters: Type::object([(
                "task",
                Type::string().with("description", "What the subagent should do"),
            )])
            .to_value(),
            // Safe to rerun after a crash: a rerun finds the child it already created and the submission it already made.
            replay: Some(ToolReplayMode::Safe),
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|args, api, call_context| {
                Box::pin(async move {
                    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                    let task_id = api.task_id();
                    // The child is owned by this tool call's task, so aborting the call aborts the child, and the call
                    // finishes only once the child's work is done.
                    let child = api
                        .commit(
                            move |tx| async move {
                                // Ownership records the child: a rerun of this call finds it instead of creating another.
                                let existing = tx
                                    .scan_conversations(
                                        ConversationQuery {
                                            owner_conversation_id: None,
                                            owner_task_id: Some(task_id),
                                        },
                                        1,
                                        None,
                                    )
                                    .await?;
                                if let Some(found) = existing.items.into_iter().next() {
                                    return Ok(found.id);
                                }
                                // Starts as a copy of this conversation's agent: model, thinking level, cwd, extensions, tools.
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                // Without this extension, the child is not offered this tool.
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                                            add: None,
                                            remove: Some(vec!["subagent".to_owned()]),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;

                    // A UI watching the parent sees this and can attach to the child.
                    api.details(&SubagentDetails { conversation_id: child }, call_context.clone())
                        .await?;

                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .ok_or_else(|| pi_js::Error::msg("missing subagent child conversation"))?;
                    // The request ID makes a rerun get back the submission it made before the crash.
                    let settled = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: Some(format!("subagent:{task_id}")),
                                content: UserInput::Text(task),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?
                        .wait(call_context.clone())
                        .await?;

                    let SubmissionState::Input(InputSubmissionState::Done { answer, .. }) = settled.record.state else {
                        return Err(pi_js::Error::msg(format!(
                            "Subagent failed: {:?}",
                            settled.record.status()
                        )));
                    };
                    let text = answer_text(&api, answer, call_context).await?;
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(text))]),
                        details: Some(JsonValue::Object(serde_json::Map::from_iter([(
                            "conversationId".to_owned(),
                            JsonValue::from(child.as_i64()),
                        )]))),
                        ..Default::default()
                    })
                })
            }),
        })],
        ..Default::default()
    })
}

fn print_event(indent: &str, event: &AgentEvent) {
    match event {
        AgentEvent::MessageEnd { entry } if ASSISTANT_ENTRY.matches(Some(entry)) => {
            if let Some(Message::Assistant(msg)) = entry.model.as_ref().and_then(|m| m.first()) {
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    pi_js::console::log(&format!("{indent}assistant: {text}"));
                }
            }
        }
        AgentEvent::ToolExecutionStart { tool_name, args, .. } => {
            pi_js::console::log(&format!("{indent}tool {tool_name}({})", pi_js::json::stringify(args)));
        }
        _ => {}
    }
}

fn attach<'a>(
    harness: &'a Harness,
    id: ConversationId,
    indent: String,
    attached: Arc<Mutex<IndexSet<ConversationId>>>,
    streams: Arc<Mutex<Vec<AgentEventStream>>>,
    context: Context,
) -> Pin<Box<dyn Future<Output = pi_js::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        {
            let mut guard = attached.lock().unwrap_or_else(|e| e.into_inner());
            if !guard.insert(id) {
                return Ok(());
            }
        }
        let Some(conv) = harness.conversation(id, context.clone()).await? else {
            return Ok(());
        };
        let stream = watch_events(&conv, context.clone()).await?;
        let harness_c = harness.clone();
        let attached_c = Arc::clone(&attached);
        let streams_c = Arc::clone(&streams);
        let indent_c = indent.clone();
        let ctx_c = context.clone();
        stream.start(move |events, _event_ctx| {
            let harness_c = harness_c.clone();
            let attached_c = Arc::clone(&attached_c);
            let streams_c = Arc::clone(&streams_c);
            let indent_c = indent_c.clone();
            let ctx_c = ctx_c.clone();
            Box::pin(async move {
                for event in &events {
                    print_event(&indent_c, event);
                    if let AgentEvent::ToolExecutionUpdate {
                        details: Some(details), ..
                    } = event
                    {
                        if let Ok(parsed) = serde_json::from_value::<SubagentDetails>(details.clone()) {
                            let child = parsed.conversation_id;
                            let already = attached_c.lock().unwrap_or_else(|e| e.into_inner()).contains(&child);
                            if !already {
                                attach(
                                    &harness_c,
                                    child,
                                    format!("{indent_c}  "),
                                    Arc::clone(&attached_c),
                                    Arc::clone(&streams_c),
                                    ctx_c.clone(),
                                )
                                .await?;
                            }
                        }
                    }
                }
                Ok(())
            })
        })?;
        streams.lock().unwrap_or_else(|e| e.into_inner()).push(stream);
        Ok(())
    })
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // The parent delegates, the child answers, and the parent reports.
    let faux = faux_provider(None);
    let models = create_models(None);
    models.set_provider(faux.provider());
    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    let delegate = faux_tool_call(
        "subagent",
        IndexMap::from([(
            "task".to_owned(),
            JsonValue::String("Name three prime numbers.".to_owned()),
        )]),
        Some(FauxToolCallOptions {
            id: Some("call-1".to_owned()),
        }),
    );
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(delegate)),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("2, 3, and 5."))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("The subagent says: 2, 3, and 5."))),
            None,
        )),
    ]);

    let registry = create_registry();
    registry.install(subagent_extension())?;
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: None,
        },
        context.clone(),
    )
    .await?;
    let _ = AGENT_DOC.definition.kind.as_str();
    let root = harness
        .root(
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(model),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    let attached = Arc::new(Mutex::new(IndexSet::new()));
    let streams = Arc::new(Mutex::new(Vec::new()));
    attach(
        &harness,
        root.id(),
        String::new(),
        attached,
        Arc::clone(&streams),
        context.clone(),
    )
    .await?;

    let submission = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(
                    "Use the subagent tool to find three prime numbers, then tell me what it said.".to_owned(),
                ),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    submission.wait(context.clone()).await?;
    harness.wait_for_idle(context.clone()).await?;
    // Event callbacks run after their commit; let the last ones print.
    pi_js::time::sleep(0, None).await?;
    let open_streams: Vec<AgentEventStream> = {
        let mut guard = streams.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    };
    for stream in open_streams {
        stream.stop().await;
    }
    harness.close(context).await?;
    Ok(())
}

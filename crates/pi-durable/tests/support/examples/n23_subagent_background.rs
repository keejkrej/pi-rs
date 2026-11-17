//! Port of packages/durable/test/examples/23-subagent-background.ts
#![allow(dead_code)]

// Persistent background subagents. One `subagent` tool lets the main agent start named subagents, message them
// (steer or follow up), stop them mid-answer, and list them. Subagents keep working while the main agent
// answers the user, and each answer is delivered back to the main agent as a new message once it arrives. Everything
// survives a restart: the example closes the Harness while a subagent works and reopens it.
// PORT: Offline adaptation omitting OPENAI_API_KEY; uses the scripted faux model and TempDir SQLite storage.
// Run via `spec_usage::examples_smoke`.

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, TextContent, UserContentBlock, UserMessageContent,
};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    RegisterFauxProviderOptions, Type, create_models, faux_assistant_message, faux_provider, faux_text, faux_tool_call,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
use pi_durable::harness::events::{AgentEvent, AgentEventStream};
use pi_durable::harness::types::ToolExecutionResult;
use pi_durable::harness::types::ToolResultContentBlock;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{AnyTask, Task, TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, EntryId, InputSubmissionState, JsonValue, LatestFork, NextTaskState,
    Storage, SubmissionState, TaskId, TaskOptions, TaskOutcome, TaskOwnership,
};
use pi_durable::{
    AgentChange, AgentExtensionsChange, Conversation, Extension, FieldChange, Harness, HarnessOptions,
    InputSubmissionDraft, LIVE_DOC, ModelRef, Registry, RegistryReader, RootConversationOptions, SubmissionDraft,
    ToolRegistration, ToolReplayMode, UserInput, WhenBusy, configure, create_registry, define_extension, define_tool,
    watch_events,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentRecord {
    conversation_id: ConversationId,
    /// Answers already reported to the main agent: several messages can end in one answer, reported once.
    reported: Vec<EntryId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SubagentsDoc {
    agents: IndexMap<String, SubagentRecord>,
    reporters: IndexMap<String, TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AnchorState {
    phase: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReporterInput {
    name: String,
    conversation_id: ConversationId,
    message: String,
    follow_up: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum ReporterState {
    Deliver,
    Report {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentDetails {
    name: String,
    conversation_id: ConversationId,
}

fn text_of_assistant(message: Option<&AssistantMessage>) -> String {
    let Some(msg) = message else {
        return String::new();
    };
    msg.content
        .iter()
        .filter_map(|c| match c {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

fn user_content_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect(),
    }
}

fn build_subagents_doc() -> pi_js::Result<ConversationDocToken<SubagentsDoc>> {
    define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "app.subagents",
            version: 1,
            initial: SubagentsDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )
}

fn build_anchor_task() -> Task<(), AnchorState, (), ()> {
    let mut phases: IndexMap<String, TypedPhaseHandler<(), AnchorState, (), ()>> = IndexMap::new();
    phases.insert(
        "done".to_owned(),
        Arc::new(|_anchor, runtime, task_context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed { result: () },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    define_task(TaskDefinition {
        name: "app.subagent-anchor".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(AnchorState {
                phase: "done".to_owned(),
            })
        }),
        phases,
        abort: Arc::new(|_anchor, runtime, task_context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: PhantomData,
    })
}

fn build_reporter_task(subagents: ConversationDocToken<SubagentsDoc>) -> Task<ReporterInput, ReporterState, (), ()> {
    let mut phases: IndexMap<String, TypedPhaseHandler<ReporterInput, ReporterState, (), ()>> = IndexMap::new();
    let subagents_deliver = subagents;
    phases.insert(
        "deliver".to_owned(),
        Arc::new(move |reporter, runtime, task_context| {
            let subagents_tok = subagents_deliver.clone();
            Box::pin(async move {
                let ReporterInput {
                    name,
                    conversation_id,
                    message,
                    follow_up,
                } = reporter.input;
                let subagent = runtime
                    .conversation(conversation_id, task_context.clone())
                    .await?
                    .ok_or_else(|| pi_js::Error::msg("missing subagent conversation"))?;
                let submission = subagent
                    .submit(
                        InputSubmissionDraft {
                            request_id: Some(format!("subagent:{}", reporter.id)),
                            content: UserInput::Text(message),
                            when_busy: Some(if follow_up { WhenBusy::FollowUp } else { WhenBusy::Steer }),
                        },
                        task_context.clone(),
                    )
                    .await?;
                let settled = submission.wait(task_context.clone()).await?;
                let main_id = runtime.conversation_id()?;
                runtime
                    .commit(
                        move |tx, _current| {
                            let subagents_tok = subagents_tok.clone();
                            Box::pin(async move {
                                let next = |report: Option<String>| {
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: ReporterState::Report { report },
                                    }))
                                };
                                match settled.record.state {
                                    SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => {
                                        return next(if reason == "aborted" {
                                            None
                                        } else {
                                            Some(format!("[subagent {name} failed: {reason}]"))
                                        });
                                    }
                                    SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => {
                                        let draft = tx.doc_conversation(&subagents_tok, main_id).await?;
                                        let mut already_reported = false;
                                        draft.update(|doc| {
                                            if let Some(agent) = doc.agents.get_mut(&name) {
                                                if agent.reported.contains(&answer) {
                                                    already_reported = true;
                                                } else {
                                                    agent.reported.push(answer);
                                                }
                                            } else {
                                                already_reported = true;
                                            }
                                        })?;
                                        if already_reported {
                                            return next(None);
                                        }
                                        let entry = tx.entry(answer).await?;
                                        let msg = entry.and_then(|e| e.model).and_then(|mut m| m.drain(..).next());
                                        let answer_ref = match &msg {
                                            Some(Message::Assistant(a)) => Some(a),
                                            _ => None,
                                        };
                                        next(Some(format!(
                                            "[subagent {name} answered, no reply needed] {}",
                                            text_of_assistant(answer_ref)
                                        )))
                                    }
                                    _ => next(None),
                                }
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "report".to_owned(),
        Arc::new(|reporter, runtime, task_context| {
            Box::pin(async move {
                let report = match &reporter.checkpoint {
                    ReporterState::Report { report } => report.clone(),
                    ReporterState::Deliver => None,
                };
                if let Some(report) = report {
                    let main_id = runtime.conversation_id()?;
                    let main = runtime
                        .conversation(main_id, task_context.clone())
                        .await?
                        .ok_or_else(|| pi_js::Error::msg("missing main conversation"))?;
                    main.submit(
                        InputSubmissionDraft {
                            request_id: Some(format!("subagent-report:{}", reporter.id)),
                            content: UserInput::Text(report),
                            when_busy: Some(WhenBusy::FollowUp),
                        },
                        task_context.clone(),
                    )
                    .await?;
                }
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed { result: () },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
    );

    define_task(TaskDefinition {
        name: "app.subagent-reporter".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(ReporterState::Deliver)),
        phases,
        abort: Arc::new(|_reporter, runtime, task_context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        task_context,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: PhantomData,
    })
}

fn build_subagent_tools(
    subagents: ConversationDocToken<SubagentsDoc>,
    anchor: Task<(), AnchorState, (), ()>,
    reporter: Task<ReporterInput, ReporterState, (), ()>,
) -> Extension {
    let subagents_tok = subagents;
    let anchor_tok = anchor.clone();
    let reporter_tok = reporter.clone();
    let subagent_tool = define_tool(ToolRegistration {
        name: "subagent".to_owned(),
        description: "Manage persistent subagents that work in the background. Actions: spawn (name, message), send (name, message; followUp: true queues it after the current answer instead of steering), stop (name: aborts its current work), status (name, or all subagents without one). Answers are reported back to you when they arrive.".to_owned(),
        parameters: Type::object([
            (
                "action",
                Type::union([
                    Type::literal("spawn"),
                    Type::literal("send"),
                    Type::literal("stop"),
                    Type::literal("status"),
                ]),
            ),
            ("name", Type::string().optional()),
            ("message", Type::string().optional()),
            ("followUp", Type::boolean().optional()),
        ])
        .to_value(),
        // A call interrupted by a crash is not rerun: repeating `stop` could stop newer work.
        replay: Some(ToolReplayMode::Unsafe),
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |args, api, call_context| {
            let subagents_tok = subagents_tok.clone();
            let anchor_tok = anchor_tok.clone();
            let reporter_tok = reporter_tok.clone();
            Box::pin(async move {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("").to_owned();
                let name = args.get("name").and_then(|v| v.as_str()).map(str::to_owned);
                let message = args.get("message").and_then(|v| v.as_str()).map(str::to_owned);
                let follow_up = args.get("followUp").and_then(|v| v.as_bool()).unwrap_or(false);

                let reply = |text: String, conversation_id: Option<ConversationId>| {
                    let details = conversation_id.and_then(|cid| {
                        name.as_ref().map(|n| {
                            JsonValue::Object(serde_json::Map::from_iter([
                                ("name".to_owned(), JsonValue::String(n.clone())),
                                ("conversationId".to_owned(), JsonValue::from(cid.as_i64())),
                            ]))
                        })
                    });
                    ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(text))]),
                        details,
                        ..Default::default()
                    }
                };

                let conv_id = api.conversation_id();
                let registry_doc = api
                    .read()
                    .snapshot_conversation(&subagents_tok, conv_id, call_context.clone())
                    .await?
                    .unwrap_or_default();

                if action == "status" {
                    let names: Vec<String> = match &name {
                        None => registry_doc.agents.keys().cloned().collect(),
                        Some(n) => vec![n.clone()],
                    };
                    let mut lines = Vec::new();
                    for each in names {
                        let Some(found) = registry_doc.agents.get(&each) else {
                            continue;
                        };
                        let busy = api
                            .read()
                            .snapshot_conversation(&*LIVE_DOC, found.conversation_id, call_context.clone())
                            .await?
                            .and_then(|live| live.run)
                            .is_some();
                        lines.push(format!("{each}: {}", if busy { "working" } else { "idle" }));
                    }
                    return Ok(reply(
                        if lines.is_empty() {
                            "No subagents.".to_owned()
                        } else {
                            lines.join("\n")
                        },
                        None,
                    ));
                }

                let Some(agent_name) = name.clone() else {
                    return Ok(reply(format!("{action} needs a name."), None));
                };
                let existing_agent = registry_doc.agents.get(&agent_name).cloned();
                if action != "spawn" && existing_agent.is_none() {
                    return Ok(reply(format!("No subagent named {agent_name}."), None));
                }

                if action == "stop" {
                    let target = existing_agent.expect("checked above");
                    let handle = api
                        .conversation(target.conversation_id, call_context.clone())
                        .await?
                        .ok_or_else(|| pi_js::Error::msg("missing subagent conversation"))?;
                    handle.abort(call_context, None).await?;
                    return Ok(reply(format!("Stopped {agent_name}."), Some(target.conversation_id)));
                }

                let Some(message_text) = message else {
                    return Ok(reply(format!("{action} needs a message."), None));
                };

                let subagents_commit = subagents_tok.clone();
                let anchor_commit = anchor_tok.clone();
                let reporter_commit = reporter_tok.clone();
                let task_id = api.task_id();
                let agent_name_c = agent_name.clone();
                let action_c = action.clone();
                let result_text = api
                    .commit(
                        move |tx| async move {
                            let draft = tx.doc_conversation(&subagents_commit, conv_id).await?;
                            let background = TaskOptions {
                                ownership: TaskOwnership::Conversation,
                                conversation_id: Some(conv_id),
                                background: true,
                            };
                            if action_c == "spawn" {
                                let already = draft.get()?.agents.contains_key(&agent_name_c);
                                if already {
                                    return Ok(format!("{agent_name_c} already exists; use send."));
                                }
                                let anchor_id = tx.create_task(&anchor_commit, &(), background).await?;
                                let child = tx
                                    .create_conversation(ConversationOwnership::Task {
                                        task_id: anchor_id.erased(),
                                    })
                                    .await?;
                                configure(
                                    &tx,
                                    child.id,
                                    &AgentChange {
                                        extensions: FieldChange::Set(AgentExtensionsChange::Patch {
                                            add: None,
                                            remove: Some(vec!["subagent-tools".to_owned()]),
                                        }),
                                        instructions: FieldChange::Set(format!(
                                            "You are the subagent \"{agent_name_c}\". Answer the main agent's requests."
                                        )),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                draft.update(|state| {
                                    state.agents.insert(
                                        agent_name_c.clone(),
                                        SubagentRecord {
                                            conversation_id: child.id,
                                            reported: Vec::new(),
                                        },
                                    );
                                })?;
                            }
                            let target_cid = draft
                                .get()?
                                .agents
                                .get(&agent_name_c)
                                .map(|a| a.conversation_id)
                                .ok_or_else(|| pi_js::Error::msg("subagent missing after spawn"))?;
                            let agent_name_reply = agent_name_c.clone();
                            let reporter_id = tx
                                .create_task(
                                    &reporter_commit,
                                    &ReporterInput {
                                        name: agent_name_c,
                                        conversation_id: target_cid,
                                        message: message_text,
                                        follow_up: action_c == "send" && follow_up,
                                    },
                                    background,
                                )
                                .await?;
                            draft.update(|state| {
                                state.reporters.insert(task_id.to_string(), reporter_id.erased());
                            })?;
                            Ok(if action_c == "send" {
                                format!("Sent to {agent_name_reply}.")
                            } else {
                                format!("Started {agent_name_reply}.")
                            })
                        },
                        call_context.clone(),
                    )
                    .await?;

                let current = api
                    .read()
                    .snapshot_conversation(&subagents_tok, conv_id, call_context)
                    .await?
                    .and_then(|doc| doc.agents.get(&agent_name).map(|a| a.conversation_id));
                Ok(reply(result_text, current))
            })
        }),
    });

    define_extension(Extension {
        name: "subagent-tools".to_owned(),
        tasks: vec![AnyTask::from(anchor), AnyTask::from(reporter)],
        tools: vec![subagent_tool],
        ..Default::default()
    })
}

fn route_response(request: &pi_ai::types::TranscriptContext) -> AssistantMessage {
    let call = |pairs: &[(&str, &str)]| {
        let mut map = IndexMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_owned(), JsonValue::String((*v).to_owned()));
        }
        faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("subagent", map, None))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )
    };
    let answer = |text: String| {
        faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(&text))),
            None,
        )
    };

    let Some(last) = request.messages.iter().rev().find(|m| !matches!(m, Message::System(_))) else {
        return answer("Noted.".to_owned());
    };
    match last {
        Message::ToolResult(r) => {
            let text: String = r
                .content
                .iter()
                .filter_map(|p| match p {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect();
            answer(format!("OK. {text}"))
        }
        Message::User(u) => {
            let text = user_content_text(&u.content);
            if text.contains("Start a subagent") {
                return call(&[
                    ("action", "spawn"),
                    ("name", "reader"),
                    ("message", "Summarize the plot of Moby Dick."),
                ]);
            }
            if text.contains("whale's name") {
                return call(&[
                    ("action", "send"),
                    ("name", "reader"),
                    ("message", "What is the whale called?"),
                ]);
            }
            if text.contains("every chapter") {
                return call(&[
                    ("action", "send"),
                    ("name", "reader"),
                    ("message", "Now go through all chapters in detail."),
                ]);
            }
            if text.contains("Stop reader") {
                return call(&[("action", "stop"), ("name", "reader")]);
            }
            if text.contains("my subagents") {
                return call(&[("action", "status")]);
            }
            if text.contains("[subagent") {
                return answer("Noted.".to_owned());
            }
            if text.contains("Summarize the plot") {
                return answer("A whale, a captain, an obsession.".to_owned());
            }
            if text.contains("whale called") {
                return answer("Moby Dick.".to_owned());
            }
            let chapters: Vec<String> = (1..=135).map(|idx| format!("Chapter {idx}: more whaling.")).collect();
            answer(chapters.join("\n"))
        }
        _ => answer("Noted.".to_owned()),
    }
}

fn format_user_line(text: &str) {
    if let Some(rest) = text.strip_prefix("[subagent ") {
        if let Some((header, tail)) = rest.split_once(']') {
            let mut parts = header.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("subagent");
            let status = parts.next().unwrap_or("").trim();
            let body = tail.trim_start();
            let shown = if body.is_empty() { status } else { body };
            pi_js::console::log(&format!("\n> {name}: {shown}"));
            return;
        }
    }
    pi_js::console::log(&format!("\n> {text}"));
}

async fn follow(root: &Conversation, context: Context) -> pi_js::Result<AgentEventStream> {
    let stream = watch_events(root, context).await?;
    stream.start(|events, _ctx| {
        Box::pin(async move {
            for event in &events {
                if let AgentEvent::MessageEnd { entry } = event {
                    match entry.model.as_ref().and_then(|m| m.first()) {
                        Some(Message::User(u)) => format_user_line(&user_content_text(&u.content)),
                        Some(Message::Assistant(a)) => {
                            for part in &a.content {
                                if let AssistantContentBlock::ToolCall(tc) = part {
                                    let action = tc.arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
                                    let name = tc.arguments.get("name").and_then(|v| v.as_str());
                                    let sent = tc.arguments.get("message").and_then(|v| v.as_str());
                                    let name_part = name.map(|n| format!(" {n}")).unwrap_or_default();
                                    let sent_part = sent
                                        .map(|s| format!(" {}", pi_js::json::stringify(s)))
                                        .unwrap_or_default();
                                    pi_js::console::log(&format!("  {} {action}{name_part}{sent_part}", tc.name));
                                }
                            }
                            let text = text_of_assistant(Some(a));
                            if !text.is_empty() {
                                pi_js::console::log(&text);
                            }
                        }
                        Some(Message::ToolResult(r)) => {
                            let content: String = r
                                .content
                                .iter()
                                .filter_map(|p| match p {
                                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                                    UserContentBlock::Image(_) => None,
                                })
                                .collect();
                            pi_js::console::log(&format!("  -> {content}"));
                        }
                        _ => {}
                    }
                }
            }
            Ok(())
        })
    })?;
    Ok(stream)
}

async fn until<F, Fut>(check: F) -> pi_js::Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = pi_js::Result<bool>>,
{
    for _ in 0..1000 {
        if check().await? {
            return Ok(());
        }
        pi_js::time::sleep(10, None).await?;
    }
    Ok(())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let subagents = build_subagents_doc()?;
    let anchor = build_anchor_task();
    let reporter = build_reporter_task(subagents.clone());
    let subagent_ext = build_subagent_tools(subagents.clone(), anchor, reporter);

    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(50.0),
        ..Default::default()
    }));
    let models = create_models(None);
    models.set_provider(faux.provider());
    let model = ModelRef {
        provider: "faux".to_owned(),
        model_id: "faux-1".to_owned(),
    };
    let steps: Vec<FauxResponseStep> = (0..40)
        .map(|_| {
            FauxResponseStep::Factory(Arc::new(|transcript, _opts, _state, _model| {
                Box::pin(async move { Ok(route_response(&transcript)) })
            }))
        })
        .collect();
    faux.set_responses(steps);

    let registry = create_registry();
    registry.install(subagent_ext)?;
    let directory = tempfile::TempDir::new().map_err(|err| pi_js::Error::msg(err.to_string()))?;
    let db_path = directory.path().join("session.sqlite").to_string_lossy().into_owned();

    let open = || {
        let db_path = db_path.clone();
        let models = models.clone();
        let registry = Arc::clone(&registry) as Arc<dyn RegistryReader>;
        let model = model.clone();
        let context = context.clone();
        async move {
            let storage = open_node_sqlite_storage(&db_path, NodeSqliteStorageOptions::default()).await?;
            let harness = Harness::open(
                Arc::new(storage) as Arc<dyn Storage>,
                HarnessOptions {
                    models,
                    registry,
                    settings: None,
                    env: None,
                    conversation_created: None,
                    now: None,
                    on_report: None,
                },
                context.clone(),
            )
            .await?;
            let root = harness
                .root(
                    context,
                    Some(RootConversationOptions {
                        agent: Some(AgentChange {
                            model: FieldChange::Set(model),
                            ..Default::default()
                        }),
                        init: None,
                    }),
                )
                .await?;
            Ok::<_, pi_js::Error>((harness, root))
        }
    };

    let (harness, root) = open().await?;
    let stream = follow(&root, context.clone()).await?;

    let say = |harness: &Harness, root: &Conversation, text: &str| {
        let root = root.clone();
        let _ = harness;
        let text = text.to_owned();
        let context = context.clone();
        async move {
            root.submit(
                SubmissionDraft::Input {
                    request_id: None,
                    content: UserInput::Text(text),
                    when_busy: None,
                },
                context.clone(),
            )
            .await?
            .wait(context)
            .await
        }
    };

    let settle = |harness: &Harness, root: &Conversation| {
        let harness = harness.clone();
        let root = root.clone();
        let subagents = subagents.clone();
        let context = context.clone();
        async move {
            let reporters = harness
                .snapshot_conversation(&subagents, root.id(), context.clone())
                .await?
                .map(|doc| doc.reporters)
                .unwrap_or_default();
            for id in reporters.into_values() {
                harness.wait_for_task(id, context.clone()).await?;
            }
            root.wait_for_idle(context.clone()).await?;
            pi_js::time::sleep(50, None).await?;
            Ok::<_, pi_js::Error>(())
        }
    };

    let working = |harness: &Harness, root: &Conversation, name: &str| {
        let harness = harness.clone();
        let root_id = root.id();
        let subagents = subagents.clone();
        let name = name.to_owned();
        let context = context.clone();
        async move {
            let Some(doc) = harness
                .snapshot_conversation(&subagents, root_id, context.clone())
                .await?
            else {
                return Ok(false);
            };
            let Some(agent) = doc.agents.get(&name) else {
                return Ok(false);
            };
            let live = harness
                .snapshot_conversation(&*LIVE_DOC, agent.conversation_id, context)
                .await?;
            Ok(live.and_then(|l| l.run).is_some())
        }
    };

    // The main agent answers at once; the subagent's answer is reported back when it arrives.
    say(
        &harness,
        &root,
        "Start a subagent named reader that summarizes Moby Dick.",
    )
    .await?;
    settle(&harness, &root).await?;

    // A long request, stopped while the subagent is still answering.
    say(&harness, &root, "Ask reader to summarize every chapter.").await?;
    until(|| working(&harness, &root, "reader")).await?;
    say(&harness, &root, "Stop reader.").await?;
    settle(&harness, &root).await?;

    say(&harness, &root, "What are my subagents doing?").await?;
    settle(&harness, &root).await?;

    // The process stops while the subagent works on a message; after the restart its answer still arrives.
    let before = harness
        .snapshot_conversation(&subagents, root.id(), context.clone())
        .await?
        .map(|doc| doc.reporters.len())
        .unwrap_or(0);
    root.submit(
        SubmissionDraft::Input {
            request_id: None,
            content: UserInput::Text("Ask reader for the whale's name.".to_owned()),
            when_busy: None,
        },
        context.clone(),
    )
    .await?;
    until(|| async {
        let count = harness
            .snapshot_conversation(&subagents, root.id(), context.clone())
            .await?
            .map(|doc| doc.reporters.len())
            .unwrap_or(0);
        Ok(count > before)
    })
    .await?;
    stream.stop().await;
    harness.close(context.clone()).await?;
    pi_js::console::log("\n  (process restarts)");

    let (harness, root) = open().await?;
    let stream = follow(&root, context.clone()).await?;
    settle(&harness, &root).await?;
    stream.stop().await;
    harness.close(context).await?;
    Ok(())
}

//! Port of packages/coding-agent/src/experimental/vacation/vacation.ts

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_ai::types::{AssistantContentBlock, Message, TextContent};
use pi_chord::Context;
use pi_durable::harness::agent::configure;
use pi_durable::harness::types::{
    AgentChange, AgentExtensionsChange, Extension, FieldChange, InputSubmissionDraft, ToolExecutionResult,
    ToolRegistration, ToolReplayMode, ToolResultContentBlock, UserInput, WhenBusy,
};
use pi_durable::tasks::{Task, TaskDefinition, TaskRuntimeHandle};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, InputSubmissionState, NextTaskState, RunningTask,
    SubmissionState, TaskOptions, TaskOutcome, TaskOwnership,
};
use pi_durable::{define_extension, define_task, define_tool, section_with_options};
use pi_js::vendor::typebox::t;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SearchTopic {
    Weather,
    Museums,
    Trains,
}

#[derive(Deserialize)]
struct SearchArgs {
    topic: SearchTopic,
    city: String,
}

fn search_data(topic: &SearchTopic) -> (u64, &'static [&'static str]) {
    match topic {
        SearchTopic::Weather => (
            6,
            &["Saturday: 24°C and sunny", "Sunday: 21°C, a short shower around 3 pm"],
        ),
        SearchTopic::Museums => (
            10,
            &[
                "Kunsthistorisches Museum: open 10-18, book a time slot",
                "Belvedere: Klimt's The Kiss, quietest before 11",
                "Albertina: Monet to Picasso, open until 21 on Friday",
            ],
        ),
        SearchTopic::Trains => (
            30,
            &[
                "Railjet from Salzburg: 2h 22m, every 30 min",
                "Nightjet from Munich: arrives 06:20",
            ],
        ),
    }
}

/// What the research subagent gets: only `search`.
pub static SEARCH: LazyLock<Extension> = LazyLock::new(|| {
    let parameters = t::object([
        (
            "topic",
            t::union([t::literal("weather"), t::literal("museums"), t::literal("trains")]),
        ),
        ("city", t::string()),
    ])
    .to_value();

    let tool = define_tool(ToolRegistration {
        name: "search".to_string(),
        description:
            "Search travel information about a city. Topics: weather, museums, trains. Slow; call several in parallel."
                .to_string(),
        parameters,
        replay: Some(ToolReplayMode::Safe),
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args_val, api, context: Context| {
            Box::pin(async move {
                let args: SearchArgs =
                    serde_json::from_value(args_val).map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                let (seconds, results) = search_data(&args.topic);
                let steps = seconds.div_ceil(2);
                let topic_name = match args.topic {
                    SearchTopic::Weather => "weather",
                    SearchTopic::Museums => "museums",
                    SearchTopic::Trains => "trains",
                };
                for step in 1..=steps {
                    api.output(&format!(
                        "searching {topic_name} in {}: source {step}/{steps}\n",
                        args.city
                    ))?;
                    pi_js::time::sleep(2000, context.abort_signal().as_ref()).await?;
                }
                for result in results {
                    api.output(&format!("- {result}\n"))?;
                }
                Ok(ToolExecutionResult::default())
            })
        }),
    });

    define_extension(Extension {
        name: "vacation-search".to_string(),
        tools: vec![tool],
        ..Default::default()
    })
});

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchInput {
    task: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum ResearchState {
    Deliver,
    Report { report: String },
}

pub static RESEARCH_TASK: LazyLock<Task<ResearchInput, ResearchState, Option<()>, ()>> = LazyLock::new(|| {
    let mut phases: IndexMap<
        String,
        pi_durable::tasks::TypedPhaseHandler<ResearchInput, ResearchState, Option<()>, ()>,
    > = IndexMap::new();

    phases.insert(
        "deliver".to_string(),
        Arc::new(|task, runtime, context| {
            Box::pin(async move {
                let owned_slot: Arc<Mutex<Option<ConversationId>>> = Arc::new(Mutex::new(None));
                let owned_slot_tx = Arc::clone(&owned_slot);
                let erased_id = task.id.erased();
                runtime
                    .commit(
                        move |tx, _current| async move {
                            let page = tx
                                .scan_conversations(
                                    ConversationQuery {
                                        owner_conversation_id: None,
                                        owner_task_id: Some(erased_id),
                                    },
                                    1,
                                    None,
                                )
                                .await?;
                            *owned_slot_tx.lock().unwrap_or_else(PoisonError::into_inner) =
                                page.items.first().map(|c| c.id);
                            Ok(None)
                        },
                        context.clone(),
                    )
                    .await?;

                let owned = owned_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .ok_or_else(|| pi_js::Error::msg("Research task has no owned conversation"))?;
                let child = runtime
                    .conversation(owned, context.clone())
                    .await?
                    .ok_or_else(|| pi_js::Error::msg(format!("Missing research child {}", owned.as_i64())))?;
                let req = InputSubmissionDraft {
                    request_id: Some(format!("research:{}", task.id.as_i64())),
                    content: UserInput::Text(task.input.task.clone()),
                    when_busy: None,
                };
                let settled = child.submit(req, context.clone()).await?.wait(context.clone()).await?;
                runtime
                    .commit(
                        move |tx, _current| async move {
                            let report = match settled.record.state {
                                SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => {
                                    let entry = tx.entry(answer).await?;
                                    let msg = entry
                                        .and_then(|e| e.model)
                                        .and_then(|mut m| if m.is_empty() { None } else { Some(m.swap_remove(0)) });
                                    let text = match msg {
                                        Some(Message::Assistant(assistant)) => assistant
                                            .content
                                            .into_iter()
                                            .filter_map(|part| match part {
                                                AssistantContentBlock::Text(t) => Some(t.text),
                                                _ => None,
                                            })
                                            .collect(),
                                        _ => String::new(),
                                    };
                                    format!("[research report] {text}")
                                }
                                SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. })
                                | SubmissionState::Write(pi_durable::types::WriteSubmissionState::Unanswered {
                                    reason,
                                    ..
                                }) => format!("[research report] The research failed: {reason}"),
                                _ => "[research report] The research failed: ?".to_string(),
                            };
                            Ok(Some(NextTaskState::Running {
                                checkpoint: ResearchState::Report { report },
                            }))
                        },
                        context,
                    )
                    .await
            })
        }),
    );

    phases.insert(
        "report".to_string(),
        Arc::new(
            |task: RunningTask<ResearchInput, ResearchState, Option<()>>,
             runtime: TaskRuntimeHandle<ResearchInput, ResearchState, Option<()>, ()>,
             context| {
                Box::pin(async move {
                    let report = match task.checkpoint {
                        ResearchState::Report { report } => report,
                        ResearchState::Deliver => String::new(),
                    };
                    let main = runtime
                        .conversation(task.conversation_id, context.clone())
                        .await?
                        .ok_or_else(|| pi_js::Error::msg("Missing main conversation for research report"))?;
                    main.submit(
                        InputSubmissionDraft {
                            request_id: Some(format!("research-report:{}", task.id.as_i64())),
                            content: UserInput::Text(report),
                            when_busy: Some(WhenBusy::FollowUp),
                        },
                        context.clone(),
                    )
                    .await?;
                    runtime
                        .commit(
                            |_tx, _current| async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed { result: None },
                                }))
                            },
                            context,
                        )
                        .await
                })
            },
        ),
    );

    define_task(TaskDefinition {
        name: "vacation.research".to_string(),
        version: 1,
        initial: Arc::new(|_input| Ok(ResearchState::Deliver)),
        phases,
        abort: Arc::new(|_task, runtime, context| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| async move {
                            Ok(Some(NextTaskState::Terminal {
                                outcome: TaskOutcome::Aborted {
                                    reason: None,
                                    result: None,
                                },
                            }))
                        },
                        context,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
});

#[derive(Deserialize)]
struct ResearchArgs {
    task: String,
}

/// What the main conversation gets: the prompt, `research`, and the task that delivers its report.
pub static VACATION: LazyLock<Extension> = LazyLock::new(|| {
    let parameters = t::object([(
        "task",
        t::string().with("description", "What to research, with the city and dates"),
    )])
    .to_value();

    let research_tool = define_tool(ToolRegistration {
        name: "research".to_string(),
        description: "Start a research subagent in the background. It searches while you keep talking with the user; its report arrives later as a message starting with [research report].".to_string(),
        parameters,
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|args_val, api, context| {
            Box::pin(async move {
                let args: ResearchArgs = serde_json::from_value(args_val)
                    .map_err(|e| pi_js::Error::js("TypeError", e.to_string()))?;
                api.commit(
                    move |tx| async move {
                        let owner = tx
                            .create_task(
                                &*RESEARCH_TASK,
                                &ResearchInput { task: args.task },
                                TaskOptions {
                                    ownership: TaskOwnership::Conversation,
                                    conversation_id: None,
                                    background: true,
                                },
                            )
                            .await?;
                        let child = tx
                            .create_conversation(ConversationOwnership::Task {
                                task_id: owner.erased(),
                            })
                            .await?;
                        configure(
                            &tx,
                            child.id,
                            &AgentChange {
                                extensions: FieldChange::set(AgentExtensionsChange::Replace(vec![
                                    "vacation-search".to_string(),
                                ])),
                                instructions: FieldChange::set(
                                    "You are a research subagent for a vacation planner. Search weather, museums, and trains in parallel, in one step, then report the findings in a few short bullet points.".to_string(),
                                ),
                                ..Default::default()
                            },
                        )
                        .await?;
                        Ok(())
                    },
                    context,
                )
                .await?;
                Ok(ToolExecutionResult {
                    content: Some(vec![ToolResultContentBlock::Text(TextContent::new(
                        "Research started in the background.",
                    ))]),
                    ..Default::default()
                })
            })
        }),
    });

    let preamble = section_with_options(
        "preamble",
        Arc::new(|_input, _ctx| {
            Box::pin(futures::future::ready(Ok(Some(
                "You are a friendly vacation planning assistant. Keep answers short. Hand all research to the research tool; while it runs, keep chatting with the user. When a [research report] arrives, turn it into a short plan.".to_string(),
            ))))
        }),
        Some(pi_durable::harness::define::SectionOptions { tag: Some(false) }),
    );

    define_extension(Extension {
        name: "vacation".to_string(),
        tools: vec![research_tool],
        tasks: vec![RESEARCH_TASK.as_any().clone()],
        sections: vec![preamble],
        ..Default::default()
    })
});

pub fn _unused_value(_: &Value) {}

//! Port of packages/durable/test/examples/25-compaction.ts
#![allow(dead_code)]

// Compaction: a long trip-planning chat whose older messages are summarized so the model context stays small.
// Run via `spec_usage::examples_smoke`.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::types::{
    AssistantMessage, Message, StopReason, SystemMessageContent, TranscriptContext, UserContentBlock,
    UserMessageContent,
};
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    RegisterFauxProviderOptions, create_models, faux_assistant_message, faux_provider,
};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::types::HarnessSettings;
use pi_durable::types::{InputSubmissionState, SubmissionState, TaskOutcome, TaskState};
use pi_durable::{
    AgentChange, COMPACTION_ENTRY, Conversation, ConversationEntriesQuery, FieldChange, Harness, HarnessOptions,
    HarnessSettingsSnapshot, LIVE_DOC, MemoryStorage, ModelRef, PartialCompactionPolicy, RegistryReader,
    RootConversationOptions, SubmissionDraft, UserInput, create_registry,
};
use tokio::sync::watch;

struct CompactionSettings {
    background_tokens: Arc<AtomicU64>,
}

impl HarnessSettings for CompactionSettings {
    fn get(&self) -> HarnessSettingsSnapshot {
        HarnessSettingsSnapshot {
            compaction: Some(PartialCompactionPolicy {
                enabled: None,
                reserve_tokens: Some(1000),
                keep_recent_tokens: Some(400),
                background_tokens: Some(self.background_tokens.load(Ordering::Relaxed)),
            }),
            ..Default::default()
        }
    }
}

fn message_text(message: Option<&Message>) -> String {
    match message {
        None | Some(Message::System(_)) => String::new(),
        Some(Message::User(u)) => match &u.content {
            UserMessageContent::Text(s) => s.replace('\n', " "),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .find_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.replace('\n', " ")),
                    UserContentBlock::Image(_) => None,
                })
                .unwrap_or_default(),
        },
        Some(Message::Assistant(a)) => a
            .content
            .iter()
            .find_map(|b| match b {
                pi_ai::types::AssistantContentBlock::Text(t) => Some(t.text.replace('\n', " ")),
                _ => None,
            })
            .unwrap_or_default(),
        Some(Message::ToolResult(r)) => r
            .content
            .iter()
            .find_map(|b| match b {
                UserContentBlock::Text(t) => Some(t.text.replace('\n', " ")),
                UserContentBlock::Image(_) => None,
            })
            .unwrap_or_default(),
    }
}

async fn show(conversation: &Conversation, label: &str, context: Context) -> pi_js::Result<()> {
    let view = conversation.context(context.clone()).await?;
    let stored = conversation
        .entries(ConversationEntriesQuery::default(), 1000, None, context)
        .await?
        .items
        .len();
    pi_js::console::log(&format!(
        "\n{label}: {} messages in context, {stored} entries stored",
        view.messages.len()
    ));
    if let Some(compaction) = COMPACTION_ENTRY.extract(view.head.as_ref())? {
        pi_js::console::log(&format!("  ({:?} compaction summary first)", compaction.data.reason));
    }
    for message in &view.messages {
        let (role, body) = match message {
            Message::System(_) => ("system   ", "(system prompt)".to_owned()),
            Message::User(_) => ("user     ", message_text(Some(message))),
            Message::Assistant(_) => ("assistant", message_text(Some(message))),
            Message::ToolResult(_) => ("toolResul", message_text(Some(message))),
        };
        let preview: String = body.chars().take(70).collect();
        pi_js::console::log(&format!("  {role} {preview}"));
    }
    Ok(())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A fake model with a tiny 3000-token window. It answers chat messages, writes summaries when asked to summarize,
    // and once rejects a request as too long, the way real providers report a context overflow.
    let overflow_once = Arc::new(AtomicBool::new(false));
    let summaries = Arc::new(AtomicUsize::new(0));
    let hold: Arc<Mutex<Option<watch::Receiver<bool>>>> = Arc::new(Mutex::new(None));

    let responder = {
        let overflow_once = Arc::clone(&overflow_once);
        let summaries = Arc::clone(&summaries);
        let hold = Arc::clone(&hold);
        move |transcript: TranscriptContext| {
            let overflow_once = Arc::clone(&overflow_once);
            let summaries = Arc::clone(&summaries);
            let hold = Arc::clone(&hold);
            async move {
                if let Some(Message::System(sys)) = transcript.messages.first() {
                    let is_summary = match &sys.content {
                        SystemMessageContent::Text(s) => s.contains("summarization"),
                        SystemMessageContent::Blocks(b) => b.iter().any(|t| t.text.contains("summarization")),
                    };
                    if is_summary {
                        let count = summaries.fetch_add(1, Ordering::SeqCst) + 1;
                        return Ok::<AssistantMessage, pi_js::Error>(faux_assistant_message(
                            FauxAssistantMessageContent::Text(format!(
                                "## Goal\nPlan a week in Lisbon (summary #{count})."
                            )),
                            None,
                        ));
                    }
                }
                if overflow_once.swap(false, Ordering::SeqCst) {
                    return Ok(faux_assistant_message(
                        FauxAssistantMessageContent::Text(String::new()),
                        Some(FauxAssistantMessageOptions {
                            stop_reason: Some(StopReason::Error),
                            error_message: Some("prompt is too long".to_owned()),
                            ..Default::default()
                        }),
                    ));
                }
                let rx_opt = hold.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(mut rx) = rx_opt {
                    let _ = rx.wait_for(|done| *done).await;
                }
                let question = transcript.messages.iter().rev().find(|m| matches!(m, Message::User(_)));
                let details = "details ".repeat(200);
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(format!(
                        "A detailed answer to \"{}\": {details}",
                        message_text(question)
                    )),
                    None,
                ))
            }
        }
    };

    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "tiny".to_owned(),
            name: None,
            reasoning: None,
            input: None,
            input_limits: None,
            cost: None,
            context_window: Some(3000),
            max_tokens: Some(1000),
        }]),
        ..Default::default()
    }));
    let steps: Vec<FauxResponseStep> = (0..100)
        .map(|_| {
            let responder = responder.clone();
            FauxResponseStep::Factory(Arc::new(move |transcript, _opts, _state, _model| {
                Box::pin(responder(transcript))
            }))
        })
        .collect();
    faux.set_responses(steps);
    let models = create_models(None);
    models.set_provider(faux.provider());

    // Generation blocks to compact above 3000 - 1000 = 2000 tokens and starts a background compaction above 2000 - 800.
    // Settings are read at every use, so the getter trait makes `background_tokens` live.
    let background_tokens = Arc::new(AtomicU64::new(800));
    let settings: Arc<dyn HarnessSettings> = Arc::new(CompactionSettings {
        background_tokens: Arc::clone(&background_tokens),
    });
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            models,
            registry: create_registry() as Arc<dyn RegistryReader>,
            settings: Some(settings),
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
            context.clone(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "tiny".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    let ask = |question: &str| {
        let root = root.clone();
        let harness = harness.clone();
        let question = question.to_owned();
        let context = context.clone();
        async move {
            let submission = root
                .submit(
                    SubmissionDraft::Input {
                        request_id: None,
                        content: UserInput::Text(question.clone()),
                        when_busy: None,
                    },
                    context.clone(),
                )
                .await?;
            let record = submission.wait(context.clone()).await?;
            // Let a background compaction started by this turn finish, so its summary shows below.
            let running = harness
                .snapshot_conversation(&*LIVE_DOC, root.id(), context.clone())
                .await?
                .and_then(|live| live.compactions)
                .unwrap_or_default();
            for status in running {
                harness.wait_for_task(status.task_id, context.clone()).await?;
            }
            let summary = match &record.record.state {
                SubmissionState::Input(InputSubmissionState::Done { .. }) => "answered".to_owned(),
                SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => reason.clone(),
                _ => format!("{:?}", record.record.status()),
            };
            show(&root, &format!("after \"{question}\" ({summary})"), context).await
        }
    };

    // 1. A long chat: once the context crosses the background threshold, a compaction runs while the chat goes on, and
    // its summary lands at once when the conversation is idle, otherwise at the next turn boundary.
    for question in [
        "Where should we stay?",
        "What should we eat?",
        "Which day trips?",
        "Any museums?",
        "Nightlife?",
    ] {
        ask(question).await?;
    }

    // 2. A manual compaction while an answer is still being written. The summary is ready first, waits in the inbox,
    // and is placed right after the answer.
    let (finish_tx, finish_rx) = watch::channel(false);
    *hold.lock().unwrap_or_else(|e| e.into_inner()) = Some(finish_rx);
    let busy = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("How do we get around?".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    let manual = root
        .compact(Some("Keep the hotel shortlist".to_owned()), context.clone())
        .await?;
    let settled = harness.wait_for_task(manual, context.clone()).await?;
    let placement = match settled.outcome {
        TaskOutcome::Completed { result } => result.submission_id,
        _ => None,
    };
    let summary = harness
        .submission(
            placement.ok_or_else(|| pi_js::Error::msg("manual compaction missing submissionId"))?,
            context.clone(),
        )
        .await?
        .ok_or_else(|| pi_js::Error::msg("missing compaction summary submission"))?;
    pi_js::console::log(&format!(
        "\nmanual compaction finished; its summary is {:?}",
        summary.status(context.clone()).await?.status()
    ));
    let _ = finish_tx.send(true);
    busy.wait(context.clone()).await?;
    pi_js::console::log(&format!(
        "after the answer, the summary is {:?}",
        summary.wait(context.clone()).await?.record.status()
    ));
    show(&root, "after compact()", context.clone()).await?;

    // 3. The provider rejects a request as too long: generation compacts and retries it once. Background compaction is
    // turned off so the summary below is the overflow one.
    background_tokens.store(0, Ordering::Relaxed);
    ask("What should we pack?").await?;
    overflow_once.store(true, Ordering::SeqCst);
    ask("Summarize the plan for my partner").await?;

    harness.close(context).await?;
    Ok(())
}

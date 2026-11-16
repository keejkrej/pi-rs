//! Port of packages/durable/test/examples/20-inbox.ts
#![allow(dead_code)]

// The inbox: what happens to submissions while a conversation is busy.
// Run via `spec_usage::examples_smoke`.

use std::sync::Arc;

use pi_ai::{FauxAssistantMessageContent, FauxResponseStep, create_models, faux_assistant_message, faux_provider};
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::inbox::InboxItem;
use pi_durable::harness::types::{HarnessSettings, QueueMode};
use pi_durable::types::{EntryDraft, InputSubmissionState, JsonValue, SubmissionState, WriteSubmissionState};
use pi_durable::{
    AgentChange, FieldChange, Harness, HarnessOptions, HarnessSettingsSnapshot, INBOX_DOC, MemoryStorage, ModelRef,
    RegistryReader, RootConversationOptions, SubmissionDraft, SubmissionHandle, UserInput, WhenBusy, create_registry,
};
use tokio::sync::watch;

async fn report_status(name: &str, submission: &SubmissionHandle, context: Context) -> pi_js::Result<()> {
    let settled = submission.wait(context).await?;
    let reason = match &settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. })
        | SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => reason.as_str(),
        _ => "",
    };
    pi_js::console::log(&format!("{name}: {:?} {reason}", settled.record.status()));
    Ok(())
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // The first answer waits until we let it go, so the conversation stays busy while we submit more.
    let (release_tx, held_rx) = watch::channel(false);
    let slow: FauxResponseStep = FauxResponseStep::Factory(Arc::new(move |_ctx, _opts, _state, _model| {
        let mut rx = held_rx.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|released| *released).await;
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Text("Answer to the first question.".to_owned()),
                None,
            ))
        })
    }));
    let faux = faux_provider(None);
    faux.set_responses(vec![
        slow,
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("Answer to the follow-up and the steer.".to_owned()),
            None,
        )),
    ]);
    let models = create_models(None);
    models.set_provider(faux.provider());

    // Settings apply to every conversation: place every queued follow-up at once instead of one per run.
    let settings: Arc<dyn HarnessSettings> = Arc::new(HarnessSettingsSnapshot {
        follow_up_mode: Some(QueueMode::All),
        ..Default::default()
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
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;

    let first = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("First question".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;

    // While busy, input queues as a follow-up (the default) or a steer, and writes queue too.
    let follow_up = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("A follow-up".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    let steer = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("A steer".to_owned()),
                when_busy: Some(WhenBusy::Steer),
            },
            context.clone(),
        )
        .await?;
    let note = root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "app.note".to_owned(),
                    data: Some(JsonValue::String("noted while busy".to_owned())),
                    ..Default::default()
                },
            },
            context.clone(),
        )
        .await?;
    let withdrawn = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Never mind".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    // when_busy: Reject refuses instead of queueing.
    if let Err(err) = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Now or never".to_owned()),
                when_busy: Some(WhenBusy::Reject),
            },
            context.clone(),
        )
        .await
    {
        pi_js::console::log(&format!("rejected: {}", err.message()));
    }
    // A queued submission can be withdrawn until a boundary places it.
    let abort_result = withdrawn.abort(context.clone()).await?;
    pi_js::console::log(&format!("withdraw: {abort_result:?}"));

    let inbox = harness
        .snapshot_conversation(&*INBOX_DOC, root.id(), context.clone())
        .await?;
    let inbox_summary: Vec<String> = inbox
        .map(|doc| {
            doc.items
                .into_iter()
                .map(|item| match item {
                    InboxItem::Steer { id, .. } => format!("{id} steer"),
                    InboxItem::FollowUp { id, .. } => format!("{id} followUp"),
                    InboxItem::Write { id, .. } => format!("{id} write"),
                })
                .collect()
        })
        .unwrap_or_default();
    pi_js::console::log(&format!("inbox: {inbox_summary:?}"));

    // The first answer ends the run at a final boundary: the write is placed first, then the steer and the
    // follow-up, which start the next run together.
    let _ = release_tx.send(true);
    report_status("first", &first, context.clone()).await?;
    report_status("follow-up", &follow_up, context.clone()).await?;
    report_status("steer", &steer, context.clone()).await?;
    report_status("note", &note, context.clone()).await?;
    report_status("withdrawn", &withdrawn, context.clone()).await?;

    let page = root
        .entries(
            pi_durable::ConversationEntriesQuery::default(),
            20,
            None,
            context.clone(),
        )
        .await?;
    let transcript_kinds: Vec<&str> = page.items.iter().rev().map(|entry| entry.kind.as_str()).collect();
    pi_js::console::log(&format!("transcript: {transcript_kinds:?}"));
    harness.close(context).await?;
    Ok(())
}

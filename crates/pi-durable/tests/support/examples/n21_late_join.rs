//! Port of packages/durable/test/examples/21-late-join.ts
#![allow(dead_code)]

// Late join: a client attaches while a run is already underway. It gets the current state first, the conversation
// view or the snapshot event, and then only what changes after that.
// Run via `spec_usage::examples_smoke`.

use std::sync::{Arc, Mutex};

use pi_ai::types::StopReason;
use pi_ai::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, RegisterFauxProviderOptions, Type, create_models, faux_assistant_message, faux_provider,
    faux_text, faux_tool_call,
};
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_durable::harness::events::{AgentEvent, MessageChange, ToolOutputUpdate};
use pi_durable::harness::live::ToolSlotStatus;
use pi_durable::harness::types::{ConversationView, ToolExecutionResult};
use pi_durable::types::JsonValue;
use pi_durable::{
    AgentChange, Extension, FieldChange, Harness, HarnessOptions, LiveState, MemoryStorage, ModelRef, Registry,
    RegistryReader, RootConversationOptions, SubmissionDraft, ToolRegistration, UserInput, create_registry,
    define_extension, define_tool, watch_events,
};

fn live_from_view(view: &ConversationView) -> LiveState {
    view.docs
        .get("pi.live")
        .and_then(|obj| serde_json::from_value::<LiveState>(JsonValue::Object(obj.clone())).ok())
        .unwrap_or_default()
}

pub async fn run() -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();

    // A tool that prints a line every 100 ms, then a slowly streamed answer.
    let registry = create_registry();
    registry.install(define_extension(Extension {
        name: "count".to_owned(),
        tools: vec![define_tool(ToolRegistration {
            name: "count".to_owned(),
            description: "Counts to ten".to_owned(),
            parameters: Type::object::<_, &str>([]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_args, api, _ctx| {
                Box::pin(async move {
                    for n in 1..=10 {
                        api.output(&format!("{n}\n"))?;
                        pi_js::time::sleep(100, None).await?;
                    }
                    Ok(ToolExecutionResult::default())
                })
            }),
        })],
        ..Default::default()
    }))?;

    let faux = faux_provider(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(40.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "count",
                indexmap::IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("call-1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(
                "Counted to ten, and this answer streams slowly.",
            ))),
            None,
        )),
    ]);

    let models = create_models(None);
    models.set_provider(faux.provider());
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

    let submission = root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("Count to ten, then tell me.".to_owned()),
                when_busy: None,
            },
            context.clone(),
        )
        .await?;
    // Join while the tool is halfway through.
    pi_js::time::sleep(500, None).await?;

    // Structural client: the view holds the committed transcript and documents, including the running tool's output.
    let view = root.view_state(context.clone()).await?;
    let initial_view = view.value();
    let live = live_from_view(&initial_view);
    let view_kinds: Vec<&str> = initial_view.entries.iter().map(|entry| entry.kind.as_str()).collect();
    pi_js::console::log(&format!("view entries: {view_kinds:?}"));
    pi_js::console::log(&format!(
        "view tool slot: {:?} {}",
        live.tools.as_ref().and_then(|t| t.first()).map(|s| s.status),
        pi_js::json::stringify(
            &live
                .tools
                .as_ref()
                .and_then(|t| t.first())
                .and_then(|s| s.output.as_ref())
        )
    ));
    let stop_view = view.subscribe(Arc::new(|value, _ctx, _delivery| {
        Box::pin(async move {
            let live = live_from_view(&value);
            if let Some(slot) = live.tools.as_ref().and_then(|t| t.first()) {
                if slot.status == ToolSlotStatus::Running {
                    pi_js::console::log(&format!("view output now: {}", pi_js::json::stringify(&slot.output)));
                }
            }
            Ok(())
        })
    }));

    // Event client: the snapshot event carries the same state; later events apply on top of it.
    let stream = watch_events(&root, context.clone()).await?;
    let snap_tools: Vec<String> = stream
        .snapshot
        .tools
        .iter()
        .map(|slot| format!("{} {:?}", slot.name, slot.status))
        .collect();
    pi_js::console::log(&format!("snapshot tools: {snap_tools:?}"));
    let initial_output = stream
        .snapshot
        .tools
        .first()
        .and_then(|s| s.output.clone())
        .unwrap_or_default();
    let output_state = Arc::new(Mutex::new(initial_output));
    let output_cb = Arc::clone(&output_state);
    stream.start(move |events, _ctx| {
        let output_cb = Arc::clone(&output_cb);
        Box::pin(async move {
            for event in events {
                match event {
                    AgentEvent::ToolExecutionUpdate {
                        output: Some(update), ..
                    } => {
                        let next_text = {
                            let mut guard = output_cb.lock().unwrap_or_else(|e| e.into_inner());
                            let next = match update {
                                ToolOutputUpdate::Set { set } => set,
                                ToolOutputUpdate::Delta { trim_start, append } => {
                                    let trimmed = guard.chars().skip(trim_start.unwrap_or(0)).collect::<String>();
                                    format!("{trimmed}{}", append.unwrap_or_default())
                                }
                            };
                            *guard = next.clone();
                            next
                        };
                        pi_js::console::log(&format!("event output now: {}", pi_js::json::stringify(&next_text)));
                    }
                    AgentEvent::MessageUpdate { changes, .. } => {
                        let deltas: String = changes
                            .iter()
                            .filter_map(|change| match change {
                                MessageChange::TextDelta { delta, .. } => Some(delta.as_str()),
                                _ => None,
                            })
                            .collect();
                        pi_js::console::log(&format!("event text delta: {}", pi_js::json::stringify(&deltas)));
                    }
                    other => {
                        pi_js::console::log(&format!("event: {other:?}"));
                    }
                }
            }
            Ok(())
        })
    })?;

    submission.wait(context.clone()).await?;
    harness.wait_for_idle(context.clone()).await?;
    pi_js::time::sleep(0, None).await?;
    stream.stop().await;
    stop_view.call();
    view.dispose()?;
    harness.close(context).await?;
    Ok(())
}

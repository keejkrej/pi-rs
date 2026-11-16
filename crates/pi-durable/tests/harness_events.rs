//! Port of packages/durable/test/harness-events.test.ts

mod support;

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseFactory, FauxResponseStep,
    FauxTokenSize, FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text, faux_thinking,
    faux_tool_call,
};
use pi_ai::types::{AssistantContentBlock, AssistantMessage, Message, StopReason};
use pi_ai::{TSchema, Type};
use pi_chord::context::with_abort_signal;
use pi_chord::delta::Seg;
use pi_durable::harness::agent::AGENT_DOC;
use pi_durable::harness::define::define_tool;
use pi_durable::harness::events::{
    AgentEvent, AgentEventStream, InboxItemMode, MessageChange, SnapshotEvent, ToolOutputUpdate,
};
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::json::{JsonKey, assign_json};
use pi_durable::harness::live::{LIVE_DOC, LiveDeferred, LiveGeneration, LiveState, ToolSlot, ToolSlotStatus};
use pi_durable::harness::types::{
    AgentState, ConversationCreateOptions, OutputRetainMode, PartialRetryPolicy, SubmissionDraft, ToolExecutionResult,
    ToolOutputLimits, UsageState, UserInput,
};
use pi_durable::harness::usage::USAGE_DOC;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{ConversationOwnership, EntryDraft, JsonObject, JsonValue, SubmissionStatus, WatchEnd};
use pi_js::abort::AbortController;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{ChatSetup, OpenChatEnv, chat_setup, open_chat, text_of, wait_for};
use support::harness_support::add_tool;
use support::session_support::{context, document_changes, flush};
use support::task_support::{aborted, deferred};

struct Listener {
    stream: AgentEventStream,
    batches: Arc<Mutex<Vec<Vec<AgentEvent>>>>,
}

impl Listener {
    fn batches_snapshot(&self) -> Vec<Vec<AgentEvent>> {
        self.batches.lock().expect("lock batches").clone()
    }

    fn events(&self) -> Vec<AgentEvent> {
        self.batches
            .lock()
            .expect("lock batches")
            .iter()
            .flatten()
            .cloned()
            .collect()
    }
}

async fn listen(conversation: &Conversation) -> Listener {
    let stream = conversation.watch_events(context()).await.expect("watch_events");
    let batches = Arc::new(Mutex::new(Vec::new()));
    let batches_cb = Arc::clone(&batches);
    stream
        .start(move |events, _| {
            let batches_cb = Arc::clone(&batches_cb);
            Box::pin(async move {
                batches_cb.lock().expect("lock batches").push(events);
                Ok(())
            })
        })
        .expect("start");
    Listener { stream, batches }
}

async fn drained() {
    flush().await;
}

fn slow_setup() -> ChatSetup {
    chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(400.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }))
}

fn streamed_text(events: &[AgentEvent]) -> String {
    let mut text = String::new();
    for event in events {
        match event {
            AgentEvent::MessageStart {
                message: Message::Assistant(a),
            } => {
                text = match a.content.first() {
                    Some(AssistantContentBlock::Text(t)) => t.text.clone(),
                    _ => String::new(),
                };
            }
            AgentEvent::MessageUpdate { changes, .. } => {
                for change in changes {
                    match change {
                        MessageChange::TextStart {
                            content_index: 0,
                            block: AssistantContentBlock::Text(t),
                        } => {
                            text = t.text.clone();
                        }
                        MessageChange::TextDelta {
                            content_index: 0,
                            delta,
                        } => {
                            text.push_str(delta);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    text
}

fn apply_changes(message: &AssistantMessage, changes: &[MessageChange]) -> AssistantMessage {
    let mut next = message.clone();
    for change in changes {
        match change {
            MessageChange::Message { message } => {
                next = message.clone();
            }
            MessageChange::Block { content_index, block } => {
                next.content[*content_index] = block.clone();
            }
            MessageChange::TextStart { content_index, block }
            | MessageChange::ThinkingStart { content_index, block }
            | MessageChange::ToolcallStart { content_index, block } => {
                next.content.insert(*content_index, block.clone());
            }
            MessageChange::TextDelta { content_index, delta } => {
                if let Some(AssistantContentBlock::Text(t)) = next.content.get_mut(*content_index) {
                    t.text.push_str(delta);
                }
            }
            MessageChange::ThinkingDelta { content_index, delta } => {
                if let Some(AssistantContentBlock::Thinking(t)) = next.content.get_mut(*content_index) {
                    t.thinking.push_str(delta);
                }
            }
            MessageChange::ToolcallDelta {
                content_index,
                path,
                delta,
            } => {
                if let Some(AssistantContentBlock::ToolCall(tc)) = next.content.get_mut(*content_index) {
                    let mut current =
                        JsonValue::Object(tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                    if let Some((last, prefix)) = path.split_last() {
                        let mut node: &mut JsonValue = &mut current;
                        for seg in prefix {
                            match (node, seg) {
                                (JsonValue::Object(map), Seg::String(k)) => {
                                    node = map.get_mut(k).expect("path segment");
                                }
                                (JsonValue::Array(arr), Seg::Index(i)) => {
                                    node = arr.get_mut(*i as usize).expect("index segment");
                                }
                                _ => panic!("Invalid toolcall path"),
                            }
                        }
                        match (node, last) {
                            (JsonValue::Object(map), Seg::String(k)) => {
                                let prev = map.get(k).and_then(|v| v.as_str()).unwrap_or("").to_owned();
                                map.insert(k.clone(), JsonValue::String(format!("{prev}{delta}")));
                            }
                            (JsonValue::Array(arr), Seg::Index(i)) => {
                                let idx = *i as usize;
                                let prev = arr[idx].as_str().unwrap_or("").to_owned();
                                arr[idx] = JsonValue::String(format!("{prev}{delta}"));
                            }
                            _ => panic!("Invalid final toolcall path segment"),
                        }
                    }
                    if let JsonValue::Object(map) = current {
                        tc.arguments = map.into_iter().collect();
                    }
                }
            }
        }
    }
    next
}

fn partials_of(harness: &Harness) -> (Arc<Mutex<Vec<AssistantMessage>>>, pi_js::Unsubscribe) {
    let partials = Arc::new(Mutex::new(Vec::new()));
    let partials_cb = Arc::clone(&partials);
    let sub = harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(live) = serde_json::from_value::<LiveState>(JsonValue::Object(val))
                    && let Some(msg) = live.generation.and_then(|g| g.message)
                {
                    partials_cb.lock().expect("lock partials").push(msg);
                }
            }
        })
        .expect("subscribe commits");
    (partials, sub)
}

fn event_type_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::Snapshot(_) => "snapshot",
        AgentEvent::RunStart { .. } => "run_start",
        AgentEvent::RunEnd { .. } => "run_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
        AgentEvent::InboxUpdate { .. } => "inbox_update",
        AgentEvent::Submission { .. } => "submission",
        AgentEvent::AutoRetryStart { .. } => "auto_retry_start",
        AgentEvent::AutoRetryEnd { .. } => "auto_retry_end",
        AgentEvent::DeferredPoll { .. } => "deferred_poll",
        AgentEvent::EntryAppended { .. } => "entry_appended",
        AgentEvent::AgentChanged { .. } => "agent_changed",
        AgentEvent::UsageChanged { .. } => "usage_changed",
        AgentEvent::TaskFailed { .. } => "task_failed",
        AgentEvent::CompactionStart { .. } => "compaction_start",
        AgentEvent::CompactionEnd { .. } => "compaction_end",
    }
}

#[tokio::test]
async fn streams_a_run_as_lifecycle_events_and_text_deltas_that_rebuild_the_answer() {
    let setup = slow_setup();
    let text = "streamed answer text ".repeat(20);
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(&text))),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let listener = listen(&opened.root).await;
    assert_eq!(listener.stream.snapshot.entries, Vec::new());
    assert_eq!(listener.stream.snapshot.tools, Vec::new());
    assert_eq!(listener.stream.snapshot.inbox, Vec::new());

    let partials = Arc::new(Mutex::new(Vec::<String>::new()));
    let partials_cb = Arc::clone(&partials);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(live) = serde_json::from_value::<LiveState>(JsonValue::Object(val))
                    && let Some(msg) = live.generation.and_then(|g| g.message)
                {
                    let wrapped = Message::Assistant(msg);
                    partials_cb
                        .lock()
                        .expect("lock")
                        .push(text_of(&wrapped).unwrap_or("").to_owned());
                }
            }
        })
        .expect("subscribe");

    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    submission.wait(context()).await.expect("wait");
    drained().await;
    let all = listener.events();
    let mut collapsed = Vec::new();
    for ev in &all {
        let name = event_type_name(ev);
        if collapsed.last().copied() != Some(name) {
            collapsed.push(name);
        }
    }
    assert_eq!(
        collapsed,
        vec![
            "message_start",
            "message_end",
            "submission",
            "run_start",
            "turn_start",
            "message_start",
            "message_update",
            "message_end",
            "turn_end",
            "run_end",
            "submission",
            "usage_changed",
        ]
    );
    let run_start = all
        .iter()
        .find(|e| matches!(e, AgentEvent::RunStart { .. }))
        .expect("run_start");
    assert_eq!(
        run_start,
        &AgentEvent::RunStart {
            inputs: vec![submission.id()]
        }
    );
    let rebuilt: Vec<String> = all
        .iter()
        .enumerate()
        .filter_map(|(index, ev)| match ev {
            AgentEvent::MessageStart {
                message: Message::Assistant(_),
            }
            | AgentEvent::MessageUpdate { .. } => Some(streamed_text(&all[..=index])),
            _ => None,
        })
        .collect();
    let partials_list = partials.lock().expect("lock").clone();
    assert_eq!(rebuilt, partials_list);
    assert!(partials_list.len() > 1);
    let end = all
        .iter()
        .rfind(|e| matches!(e, AgentEvent::MessageEnd { .. }))
        .expect("message_end");
    match end {
        AgentEvent::MessageEnd { entry } => {
            assert_eq!(text_of(&entry.model.as_ref().expect("model")[0]), Some(text.as_str()));
        }
        _ => unreachable!(),
    }
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_tool_start_output_appends_and_the_result_entry() {
    let setup = chat_setup(None);
    let gate = deferred::<()>();
    let gate_cb = gate.clone();
    add_tool(
        &setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "print".to_owned(),
            description: "Prints".to_owned(),
            parameters: Type::object([("n", Type::number())]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_, api, _| {
                let gate_cb = gate_cb.clone();
                Box::pin(async move {
                    api.output("one\n")?;
                    pi_js::time::sleep(150, None).await?;
                    api.output("two\n")?;
                    gate_cb.wait().await?;
                    Ok(ToolExecutionResult::default())
                })
            }),
        }),
        None,
    )
    .expect("add print");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "print",
                IndexMap::from([("n".to_owned(), json!(1))]),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let listener = listen(&opened.root).await;
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");

    let rebuild_output = |evs: &[AgentEvent]| -> String {
        let mut text = String::new();
        for event in evs {
            if let AgentEvent::ToolExecutionUpdate { output: Some(out), .. } = event {
                match out {
                    ToolOutputUpdate::Set { set } => text = set.clone(),
                    ToolOutputUpdate::Delta { trim_start, append } => {
                        let start = trim_start.unwrap_or(0);
                        text = format!("{}{}", &text[start..], append.as_deref().unwrap_or(""));
                    }
                }
            }
        }
        text
    };
    wait_for(
        || {
            let batches = Arc::clone(&listener.batches);
            async move {
                let evs: Vec<AgentEvent> = batches.lock().expect("lock").iter().flatten().cloned().collect();
                rebuild_output(&evs) == "one\ntwo\n"
            }
        },
        5_000,
    )
    .await;
    gate.resolve(());
    submission.wait(context()).await.expect("wait");
    drained().await;
    let all = listener.events();
    let tool: Vec<&AgentEvent> = all
        .iter()
        .filter(|e| event_type_name(e).starts_with("tool_execution"))
        .collect();
    assert_eq!(
        tool[0],
        &AgentEvent::ToolExecutionStart {
            tool_call_id: "c1".to_owned(),
            tool_name: "print".to_owned(),
            args: json!({ "n": 1 }).as_object().expect("obj").clone(),
        }
    );
    let end = *tool.last().expect("end");
    match end {
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            entry: Some(entry),
            ..
        } => {
            assert_eq!(tool_call_id, "c1");
            assert_eq!(entry.kind, "pi.tool-result");
        }
        other => panic!("Expected ToolExecutionEnd with entry, got {other:?}"),
    }
    let end_index = all.iter().position(|e| e == end).expect("end index");
    let seq_names: Vec<&str> = all[end_index..end_index + 3].iter().map(event_type_name).collect();
    assert_eq!(seq_names, vec!["tool_execution_end", "message_start", "message_end"]);
    match &all[end_index + 1] {
        AgentEvent::MessageStart {
            message: Message::ToolResult(tr),
        } => assert_eq!(tr.tool_call_id, "c1"),
        other => panic!("Expected toolResult start, got {other:?}"),
    }
    assert_eq!(all.iter().filter(|e| matches!(e, AgentEvent::TurnStart)).count(), 2);
    assert_eq!(all.iter().filter(|e| matches!(e, AgentEvent::TurnEnd)).count(), 2);
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_queued_submissions_inbox_changes_and_retries() {
    let setup = chat_setup(None);
    let release = deferred::<()>();
    let release_cb = release.clone();
    let held: FauxResponseFactory = Arc::new(move |_, options, _, _| {
        let release_cb = release_cb.clone();
        Box::pin(async move {
            let sig = options.and_then(|o| o.base.signal);
            tokio::select! {
                _ = release_cb.wait() => Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("first"))),
                    None,
                )),
                res = async {
                    match sig {
                        Some(s) => aborted::<()>(&s).await,
                        None => std::future::pending().await,
                    }
                } => Err(res.expect_err("aborted")),
            }
        })
    });
    let error = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(Vec::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some("503 Service Unavailable".to_owned()),
            ..Default::default()
        }),
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Factory(held),
        FauxResponseStep::Message(error),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("second"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(1),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    let listener = listen(&opened.root).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    let follow_up = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f");
    drained().await;
    let inbox_updates: Vec<AgentEvent> = listener
        .events()
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::InboxUpdate { .. }))
        .collect();
    assert_eq!(
        inbox_updates,
        vec![AgentEvent::InboxUpdate {
            items: vec![pi_durable::harness::events::QueuedItem {
                id: follow_up.id(),
                mode: InboxItemMode::FollowUp,
            }],
        }]
    );
    release.resolve(());
    follow_up.wait(context()).await.expect("wait followup");
    drained().await;
    let all = listener.events();
    let types: Vec<&str> = all.iter().map(event_type_name).collect();
    assert!(types.contains(&"auto_retry_start"));
    assert!(types.contains(&"auto_retry_end"));
    assert_eq!(types.iter().filter(|&&t| t == "run_start").count(), 2);
    let sub_statuses: Vec<SubmissionStatus> = all
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Submission { record } => Some(record.status()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sub_statuses,
        vec![
            SubmissionStatus::Placed,
            SubmissionStatus::Queued,
            SubmissionStatus::Done,
            SubmissionStatus::Placed,
            SubmissionStatus::Done,
        ]
    );
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn replaces_undelivered_batches_with_one_snapshot_after_100_pending_batches() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let stream = opened.root.watch_events(context()).await.expect("watch");
    let root_id = opened.root.id();
    for _ in 0..101 {
        opened
            .root
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
            .expect("append");
    }
    let batches = Arc::new(Mutex::new(Vec::new()));
    let batches_cb = Arc::clone(&batches);
    stream
        .start(move |events, _| {
            let batches_cb = Arc::clone(&batches_cb);
            Box::pin(async move {
                batches_cb.lock().expect("lock").push(events);
                Ok(())
            })
        })
        .expect("start");
    drained().await;
    let b = batches.lock().expect("lock");
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].len(), 1);
    match &b[0][0] {
        AgentEvent::Snapshot(snap) => assert_eq!(snap.entries.len(), 101),
        other => panic!("Expected Snapshot, got {other:?}"),
    }
    drop(b);
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rebuilds_every_committed_partial_of_thinking_text_and_tool_call_arguments_from_message_changes() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(150.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    let message = faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![
            FauxContentBlock::Thinking(faux_thinking(&"thinking about it ".repeat(10))),
            FauxContentBlock::Text(faux_text(&"some text ".repeat(10))),
            FauxContentBlock::ToolCall(faux_tool_call(
                "missing",
                IndexMap::from([
                    ("path".to_owned(), json!("a/long/path/".repeat(10))),
                    ("note".to_owned(), json!("x".repeat(60))),
                ]),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            )),
        ]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    );
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(message),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let (partials, _sub) = partials_of(&opened.harness);
    let listener = listen(&opened.root).await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    drained().await;
    let mut rebuilt = Vec::new();
    let mut current: Option<AssistantMessage> = None;
    let all = listener.events();
    for event in &all {
        if let AgentEvent::MessageEnd { entry } = event
            && entry.kind == "pi.assistant"
        {
            break;
        }
        match event {
            AgentEvent::MessageStart {
                message: Message::Assistant(a),
            } => current = Some(a.clone()),
            AgentEvent::MessageUpdate { changes, .. } => {
                current = Some(apply_changes(current.as_ref().expect("current"), changes));
            }
            _ => continue,
        }
        rebuilt.push(current.clone().expect("cur"));
    }
    let partials_list = partials.lock().expect("lock").clone();
    assert!(partials_list.len() > 2);
    let rebuilt_contents: Vec<_> = rebuilt.iter().map(|p| p.content.clone()).collect();
    let partial_contents: Vec<_> = partials_list.iter().map(|p| p.content.clone()).collect();
    assert_eq!(rebuilt_contents, partial_contents);
    let change_types: IndexSet<&str> = all
        .iter()
        .flat_map(|ev| match ev {
            AgentEvent::MessageUpdate { changes, .. } => changes.as_slice(),
            _ => &[],
        })
        .map(|c| match c {
            MessageChange::ThinkingDelta { .. } => "thinking_delta",
            MessageChange::TextDelta { .. } => "text_delta",
            _ => "other",
        })
        .collect();
    assert!(change_types.contains("thinking_delta") || change_types.contains("text_delta"));
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rebuilds_a_sliding_tail_window_from_output_trims_and_appends() {
    let setup = chat_setup(None);
    let gate = deferred::<()>();
    let gate_cb = gate.clone();
    add_tool(
        &setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "tail".to_owned(),
            description: "Prints lines".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: Some(ToolOutputLimits {
                max_lines: Some(3),
                max_bytes: None,
                retain: Some(OutputRetainMode::Tail),
            }),
            execute: Arc::new(move |_, api, _| {
                let gate_cb = gate_cb.clone();
                Box::pin(async move {
                    for line in 0..6 {
                        api.output(&format!("line {line}\n"))?;
                        pi_js::time::sleep(120, None).await?;
                    }
                    gate_cb.wait().await?;
                    Ok(ToolExecutionResult::default())
                })
            }),
        }),
        None,
    )
    .expect("add tail");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "tail",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let outputs = Arc::new(Mutex::new(Vec::<String>::new()));
    let outputs_cb = Arc::clone(&outputs);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(live) = serde_json::from_value::<LiveState>(JsonValue::Object(val))
                    && let Some(out) = live.tools.and_then(|t| t.into_iter().next()).and_then(|s| s.output)
                {
                    let mut guard = outputs_cb.lock().expect("lock outputs");
                    if guard.last() != Some(&out) {
                        guard.push(out);
                    }
                }
            }
        })
        .expect("subscribe");
    let listener = listen(&opened.root).await;
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    wait_for(
        || {
            let outputs = Arc::clone(&outputs);
            async move {
                outputs
                    .lock()
                    .expect("lock")
                    .last()
                    .is_some_and(|s| s.ends_with("line 5\n"))
            }
        },
        5_000,
    )
    .await;
    gate.resolve(());
    submission.wait(context()).await.expect("wait");
    drained().await;
    let mut rebuilt = Vec::new();
    let mut text = String::new();
    let all = listener.events();
    for event in &all {
        if let AgentEvent::ToolExecutionUpdate { output: Some(out), .. } = event {
            match out {
                ToolOutputUpdate::Set { set } => text = set.clone(),
                ToolOutputUpdate::Delta { trim_start, append } => {
                    let start = trim_start.unwrap_or(0);
                    text = format!("{}{}", &text[start..], append.as_deref().unwrap_or(""));
                }
            }
            rebuilt.push(text.clone());
        }
    }
    assert_eq!(rebuilt, *outputs.lock().expect("lock"));
    assert!(all.iter().any(|ev| matches!(
        ev,
        AgentEvent::ToolExecutionUpdate {
            output: Some(ToolOutputUpdate::Delta {
                trim_start: Some(_),
                ..
            }),
            ..
        }
    )));
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn emits_one_exact_batch_when_a_run_ends_and_a_queued_follow_up_starts_the_next() {
    let setup = chat_setup(None);
    let release = deferred::<()>();
    let release_cb = release.clone();
    let held: FauxResponseFactory = Arc::new(move |_, options, _, _| {
        let release_cb = release_cb.clone();
        Box::pin(async move {
            let sig = options.and_then(|o| o.base.signal);
            tokio::select! {
                _ = release_cb.wait() => Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("first"))),
                    None,
                )),
                res = async {
                    match sig {
                        Some(s) => aborted::<()>(&s).await,
                        None => std::future::pending().await,
                    }
                } => Err(res.expect_err("aborted")),
            }
        })
    });
    setup.faux.set_responses(vec![
        FauxResponseStep::Factory(held),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("second"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    let follow_up = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("f".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit f");
    let listener = listen(&opened.root).await;
    release.resolve(());
    follow_up.wait(context()).await.expect("wait");
    drained().await;
    let batches = listener.batches_snapshot();
    let boundary = batches
        .iter()
        .find(|batch| batch.iter().any(|e| matches!(e, AgentEvent::RunEnd { .. })))
        .expect("boundary batch");
    let boundary_types: Vec<&str> = boundary.iter().map(event_type_name).collect();
    assert_eq!(
        boundary_types,
        vec![
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "run_end",
            "submission",
            "submission",
            "inbox_update",
            "usage_changed",
            "run_start",
            "turn_start",
        ]
    );
    let sub_ids: Vec<_> = boundary
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Submission { record } => Some(record.id),
            _ => None,
        })
        .collect();
    assert_eq!(sub_ids, vec![input.id(), follow_up.id()]);
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_a_call_that_never_runs_and_a_tool_aborted_with_its_generation() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "wait".to_owned(),
            description: "Waits until aborted".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, call_ctx| {
                Box::pin(async move { aborted(call_ctx.abort_signal().as_ref().expect("abort signal")).await })
            }),
        }),
        None,
    )
    .expect("add wait");
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::ToolCall(faux_tool_call(
                    "ghost",
                    IndexMap::new(),
                    Some(FauxToolCallOptions {
                        id: Some("c1".to_owned()),
                    }),
                )),
                FauxContentBlock::ToolCall(faux_tool_call(
                    "wait",
                    IndexMap::new(),
                    Some(FauxToolCallOptions {
                        id: Some("c2".to_owned()),
                    }),
                )),
            ]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let listener = listen(&opened.root).await;
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    wait_for(
        || {
            let batches = Arc::clone(&listener.batches);
            async move {
                batches
                    .lock()
                    .expect("lock")
                    .iter()
                    .flatten()
                    .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
            }
        },
        5_000,
    )
    .await;
    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("present");
    opened
        .harness
        .abort_task(live.run.expect("run").task_id, context())
        .await
        .expect("abort");
    submission.wait(context()).await.expect("wait");
    drained().await;
    let all = listener.events();
    let round: Vec<String> = all
        .iter()
        .map(|ev| match ev {
            AgentEvent::MessageEnd { entry } => format!("end:{}", entry.kind),
            AgentEvent::MessageStart { message } => {
                let role = match message {
                    Message::User(_) => "user",
                    Message::Assistant(_) => "assistant",
                    Message::System(_) => "system",
                    Message::ToolResult(_) => "toolResult",
                };
                format!("start:{role}")
            }
            other => event_type_name(other).to_owned(),
        })
        .collect();
    let ghost_end = round.iter().position(|r| r == "tool_execution_end").expect("ghost end");
    assert_eq!(
        &round[ghost_end - 1..ghost_end + 3],
        &[
            "end:pi.assistant",
            "tool_execution_end",
            "start:toolResult",
            "end:pi.tool-result"
        ]
    );
    let tool_tuples: Vec<(&str, &str, bool)> = all
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                Some(("tool_execution_start", tool_call_id.as_str(), false))
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id, entry, ..
            } => Some(("tool_execution_end", tool_call_id.as_str(), entry.is_some())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_tuples,
        vec![
            ("tool_execution_end", "c1", true),
            ("tool_execution_start", "c2", false),
            ("tool_execution_end", "c2", true),
        ]
    );
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_a_steer_without_run_events_a_reset_as_an_appended_entry_and_nothing_for_other_conversations() {
    let setup = chat_setup(None);
    let gate = deferred::<()>();
    let gate_cb = gate.clone();
    add_tool(
        &setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "hold".to_owned(),
            description: "Waits".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_, _, _| {
                let gate_cb = gate_cb.clone();
                Box::pin(async move {
                    gate_cb.wait().await?;
                    Ok(ToolExecutionResult::default())
                })
            }),
        }),
        None,
    )
    .expect("add hold");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "hold",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let other = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("other");
    let listener = listen(&opened.root).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("a".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit a");
    wait_for(
        || {
            let batches = Arc::clone(&listener.batches);
            async move {
                batches
                    .lock()
                    .expect("lock")
                    .iter()
                    .flatten()
                    .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. }))
            }
        },
        5_000,
    )
    .await;
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("s".to_owned()),
                when_busy: Some(pi_durable::harness::types::WhenBusy::Steer),
            },
            context(),
        )
        .await
        .expect("submit s");
    drained().await;
    let before = listener.batches_snapshot().len();
    let other_id = other.id();
    other
        .commit(
            move |tx| async move {
                tx.append_entry(
                    other_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("other commit");
    drained().await;
    assert_eq!(listener.batches_snapshot().len(), before);
    gate.resolve(());
    input.wait(context()).await.expect("wait");
    opened.root.reset(None, context()).await.expect("reset");
    drained().await;
    let all = listener.events();
    assert_eq!(
        all.iter().filter(|e| matches!(e, AgentEvent::RunStart { .. })).count(),
        1
    );
    assert_eq!(all.iter().filter(|e| matches!(e, AgentEvent::RunEnd { .. })).count(), 1);
    let last_batch_types: Vec<&str> = listener
        .batches_snapshot()
        .last()
        .expect("last batch")
        .iter()
        .map(event_type_name)
        .collect();
    assert_eq!(last_batch_types, vec!["entry_appended", "submission"]);
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_an_attachment_cancelled_while_it_waits_for_the_session_line() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let release = deferred::<()>();
    let release_wait = release.clone();
    let root_cb = opened.root.clone();
    let blocking = tokio::spawn(async move {
        root_cb
            .commit(move |_| async move { release_wait.wait().await }, context())
            .await
    });
    let controller = AbortController::new();
    let cancelled_ctx = with_abort_signal(controller.signal(), context());
    let root_for_watch = opened.root.clone();
    let attaching = tokio::spawn(async move { root_for_watch.watch_events(cancelled_ctx).await });
    flush().await;
    controller.abort(Some(pi_js::abort::AbortReason::from_error(&pi_js::Error::msg(
        "cancelled",
    ))));
    release.resolve(());
    blocking.await.expect("join blocking").expect("commit ok");
    let Err(err) = attaching.await.expect("join attach") else {
        panic!("expected abort error");
    };
    assert!(err.to_string().contains("cancelled"));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn applies_deltas_after_an_overflow_snapshot_that_holds_an_in_flight_partial() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let partial = faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("hel"))),
        None,
    );
    let stream = opened.root.watch_events(context()).await.expect("watch");
    let root_id = opened.root.id();
    let partial_cb = partial.clone();
    opened
        .root
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&*LIVE_DOC, root_id).await?;
                let overlay = draft.overlay()?;
                assign_json(
                    &overlay,
                    JsonKey::Prop("generation"),
                    &serde_json::to_value(LiveGeneration {
                        attempt: 1,
                        message: Some(partial_cb),
                        retry: None,
                        deferred: None,
                    })
                    .map_err(|e| pi_js::Error::msg(e.to_string()))?,
                )?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("set partial");
    for _ in 0..101 {
        opened
            .root
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
            .expect("note");
    }
    let batches = Arc::new(Mutex::new(Vec::<Vec<AgentEvent>>::new()));
    let batches_cb = Arc::clone(&batches);
    stream
        .start(move |events, _| {
            let batches_cb = Arc::clone(&batches_cb);
            Box::pin(async move {
                batches_cb.lock().expect("lock").push(events);
                Ok(())
            })
        })
        .expect("start");
    drained().await;
    let snapshot: SnapshotEvent = match &batches.lock().expect("lock")[0][0] {
        AgentEvent::Snapshot(s) => s.clone(),
        other => panic!("Expected Snapshot, got {other:?}"),
    };
    let mut updated_partial = partial;
    updated_partial.content = vec![AssistantContentBlock::Text(pi_ai::types::TextContent::new("hello"))];
    opened
        .root
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&*LIVE_DOC, root_id).await?;
                let overlay = draft.overlay()?;
                assign_json(
                    &overlay,
                    JsonKey::Prop("generation"),
                    &serde_json::to_value(LiveGeneration {
                        attempt: 1,
                        message: Some(updated_partial),
                        retry: None,
                        deferred: None,
                    })
                    .map_err(|e| pi_js::Error::msg(e.to_string()))?,
                )?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("update text");
    drained().await;
    let batches_guard = batches.lock().expect("lock");
    let update = &batches_guard.last().expect("last batch")[0];
    let changes = match update {
        AgentEvent::MessageUpdate { changes, .. } => changes,
        other => panic!("Expected MessageUpdate, got {other:?}"),
    };
    assert_eq!(
        changes,
        &vec![MessageChange::TextDelta {
            content_index: 0,
            delta: "lo".to_owned(),
        }]
    );
    let rebuilt = apply_changes(
        snapshot.generation.expect("gen").message.as_ref().expect("msg"),
        changes,
    );
    assert_eq!(
        rebuilt.content,
        vec![AssistantContentBlock::Text(pi_ai::types::TextContent::new("hello"))]
    );
    drop(batches_guard);
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_usage_only_updates_cleared_tool_progress_tools_ending_without_entries_and_deferred_polls() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let listener = listen(&opened.root).await;
    let root_id = opened.root.id();
    let partial = faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("partial"))),
        None,
    );

    let set_live = |generation: Option<LiveGeneration>, tools: Option<Vec<ToolSlot>>| {
        let root = opened.root.clone();
        async move {
            root.commit(
                move |tx| async move {
                    let draft = tx.doc_conversation(&*LIVE_DOC, root_id).await?;
                    let overlay = draft.overlay()?;
                    match generation {
                        Some(g) => assign_json(
                            &overlay,
                            JsonKey::Prop("generation"),
                            &serde_json::to_value(g).map_err(|e| pi_js::Error::msg(e.to_string()))?,
                        )?,
                        None => {
                            overlay.delete("generation")?;
                        }
                    }
                    match tools {
                        Some(t) => assign_json(
                            &overlay,
                            JsonKey::Prop("tools"),
                            &serde_json::to_value(t).map_err(|e| pi_js::Error::msg(e.to_string()))?,
                        )?,
                        None => {
                            overlay.delete("tools")?;
                        }
                    }
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit live");
        }
    };

    // 1. message_start
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: Some(partial.clone()),
            retry: None,
            deferred: None,
        }),
        None,
    )
    .await;

    // 2. usage-only change -> message_update with empty changes
    let mut partial_usage = partial.clone();
    partial_usage.usage.input = 42;
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: Some(partial_usage.clone()),
            retry: None,
            deferred: None,
        }),
        None,
    )
    .await;

    // 3. deferred poll 1
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: Some(LiveDeferred { poll_at: 1 }),
        }),
        None,
    )
    .await;

    // 4. deferred poll 2
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: Some(LiveDeferred { poll_at: 2 }),
        }),
        None,
    )
    .await;

    // 5. tool execution start
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: Some(LiveDeferred { poll_at: 2 }),
        }),
        Some(vec![ToolSlot {
            call_id: "c1".to_owned(),
            name: "t".to_owned(),
            task_id: None,
            status: ToolSlotStatus::Running,
            output: None,
            dropped_bytes: None,
            dropped_lines: None,
            details: Some(json!({ "n": 1 })),
            diagnostics: Some(Vec::new()),
            entry: None,
        }]),
    )
    .await;

    // 6. clear running slot's progress
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: Some(LiveDeferred { poll_at: 2 }),
        }),
        Some(vec![ToolSlot {
            call_id: "c1".to_owned(),
            name: "t".to_owned(),
            task_id: None,
            status: ToolSlotStatus::Running,
            output: None,
            dropped_bytes: None,
            dropped_lines: None,
            details: None,
            diagnostics: None,
            entry: None,
        }]),
    )
    .await;

    // 7. fault marks slot done without entry
    set_live(
        Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: Some(LiveDeferred { poll_at: 2 }),
        }),
        Some(vec![ToolSlot {
            call_id: "c1".to_owned(),
            name: "t".to_owned(),
            task_id: None,
            status: ToolSlotStatus::Done,
            output: None,
            dropped_bytes: None,
            dropped_lines: None,
            details: None,
            diagnostics: None,
            entry: None,
        }]),
    )
    .await;

    opened
        .root
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&*USAGE_DOC, root_id).await },
            context(),
        )
        .await
        .expect("retire usage");
    opened
        .root
        .commit(
            move |tx| async move { tx.retire_doc_conversation(&*AGENT_DOC, root_id).await },
            context(),
        )
        .await
        .expect("retire agent");
    drained().await;

    let filtered: Vec<Vec<AgentEvent>> = listener
        .batches_snapshot()
        .into_iter()
        .map(|batch| {
            batch
                .into_iter()
                .filter(|e| !matches!(e, AgentEvent::TaskFailed { .. }))
                .collect()
        })
        .collect();
    assert_eq!(
        filtered,
        vec![
            vec![AgentEvent::MessageStart {
                message: Message::Assistant(partial.clone()),
            }],
            vec![AgentEvent::MessageUpdate {
                usage: partial_usage.usage.clone(),
                changes: Vec::new(),
            }],
            vec![AgentEvent::DeferredPoll { poll_at: 1 }],
            vec![AgentEvent::DeferredPoll { poll_at: 2 }],
            vec![AgentEvent::ToolExecutionStart {
                tool_call_id: "c1".to_owned(),
                tool_name: "t".to_owned(),
                args: JsonObject::new(),
            }],
            vec![AgentEvent::ToolExecutionUpdate {
                tool_call_id: "c1".to_owned(),
                tool_name: "t".to_owned(),
                output: None,
                details: Some(JsonValue::Null),
                diagnostics: Some(Vec::new()),
            }],
            vec![AgentEvent::ToolExecutionEnd {
                tool_call_id: "c1".to_owned(),
                tool_name: "t".to_owned(),
                entry: None,
            }],
            vec![AgentEvent::UsageChanged {
                usage: UsageState::default(),
            }],
            vec![AgentEvent::AgentChanged {
                agent: AgentState::default(),
            }],
        ]
    );
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_the_stream_with_the_harness() {
    let opened = open_chat(Arc::new(MemoryStorage::new()), &chat_setup(None), OpenChatEnv::None)
        .await
        .expect("open");
    let stream = opened.root.watch_events(context()).await.expect("watch");
    opened.harness.close(context()).await.expect("close");
    assert!(matches!(stream.closed().await, WatchEnd::SessionClosed));
}

#[tokio::test]
async fn starts_a_message_at_the_first_committed_partial_and_ends_it_with_the_converted_entry_on_abort() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(100.0),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(&"x".repeat(400)))),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let listener = listen(&opened.root).await;
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    wait_for(
        || {
            let batches = Arc::clone(&listener.batches);
            async move {
                batches.lock().expect("lock").iter().flatten().any(|e| {
                    matches!(
                        e,
                        AgentEvent::MessageStart {
                            message: Message::Assistant(_)
                        }
                    )
                })
            }
        },
        5_000,
    )
    .await;
    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("present");
    opened
        .harness
        .abort_task(live.run.expect("run").task_id, context())
        .await
        .expect("abort");
    submission.wait(context()).await.expect("wait");
    drained().await;
    let all = listener.events();
    let assistant_ends = all
        .iter()
        .filter(|e| matches!(e, AgentEvent::MessageEnd { entry } if entry.kind == "pi.assistant"))
        .count();
    let assistant_starts = all
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::MessageStart {
                    message: Message::Assistant(_)
                }
            )
        })
        .count();
    assert_eq!(assistant_ends, 1);
    assert_eq!(assistant_starts, 1);
    listener.stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

//! Port of packages/durable/test/harness-live-deltas.test.ts

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxTokenSize,
    FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::StopReason;
use pi_ai::{JsonValue, TSchema, Type};
use pi_chord::Context;
use pi_chord::delta::{
    AppendVerb, DeleteVerb, Op, OpAppend, OpDelete, OpSet, OpSplice, OpTruncate, Seg, SetVerb, SpliceVerb, TruncateVerb,
};
use pi_durable::harness::define::define_tool;
use pi_durable::harness::harness::{ConversationEntriesQuery, Harness};
use pi_durable::harness::live::{LIVE_DOC, LiveGeneration, LiveRun, LiveState, ToolSlot, ToolSlotStatus};
use pi_durable::harness::types::{
    DiagnosticSeverity, OutputRetainMode, SubmissionDraft, ToolDiagnostic, ToolExecutionApi, ToolExecutionMode,
    ToolExecutionResult, ToolOutputLimits, ToolRegistration, UserInput,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::types::{
    CheckpointInfo, CommitChange, CommitPublication, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentContent, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, EntryId, EntryLookup,
    EntryQuery, EntryRecord, ErasedTaskRecord, Page, Seq, Storage, StorageWrite, StoredDocument, SubmissionId,
    SubmissionQuery, SubmissionRecord, TableCommitChange, TaskId, TaskQuery,
};
use pi_js::BoxFuture;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{OpenChatEnv, chat_setup, open_chat, wait_for};
use support::harness_support::add_tool;
use support::session_support::{ControlledStorage, context, document_changes, flush};
use support::task_support::aborted;
use tokio::sync::Notify;

type StepAction = Box<dyn FnOnce(&ToolExecutionApi) -> BoxFuture<pi_js::Result<bool>> + Send>;

fn seg_s(s: &str) -> Seg {
    Seg::String(s.to_owned())
}

fn seg_i(i: i64) -> Seg {
    Seg::Index(i)
}

fn op_s(path: Vec<Seg>, value: JsonValue) -> Op {
    Op::Set(OpSet(SetVerb::S, path, value))
}

fn op_d(path: Vec<Seg>) -> Op {
    Op::Delete(OpDelete(DeleteVerb::D, path))
}

fn op_a(path: Vec<Seg>, text: &str) -> Op {
    Op::Append(OpAppend(AppendVerb::A, path, text.to_owned()))
}

fn op_t(path: Vec<Seg>, count: i64) -> Op {
    Op::Truncate(OpTruncate(TruncateVerb::T, path, count))
}

fn op_p(path: Vec<Seg>, index: i64, remove: i64, items: Vec<JsonValue>) -> Op {
    Op::Splice(OpSplice(SpliceVerb::P, path, index, remove, items))
}

fn output_path() -> Vec<Seg> {
    vec![seg_s("tools"), seg_i(0), seg_s("output")]
}

fn is_output(op: &Op) -> bool {
    let target = output_path();
    match op {
        Op::Set(OpSet(_, p, _))
        | Op::Delete(OpDelete(_, p))
        | Op::Append(OpAppend(_, p, _))
        | Op::Truncate(OpTruncate(_, p, _))
        | Op::Splice(OpSplice(_, p, _, _, _)) => p == &target,
        _ => false,
    }
}

fn op_verb(op: &Op) -> &'static str {
    match op {
        Op::Replace(_) => "r",
        Op::Set(_) => "s",
        Op::Delete(_) => "d",
        Op::Append(_) => "a",
        Op::Truncate(_) => "t",
        Op::Splice(_) => "p",
        Op::Move(_) => "m",
    }
}

async fn live_state(harness: &Harness, root_id: ConversationId) -> Option<LiveState> {
    harness
        .snapshot_conversation(&*LIVE_DOC, root_id, context())
        .await
        .expect("snapshot live")
}

struct DrivenTool {
    harness: Harness,
    root_id: ConversationId,
    actions: Arc<Mutex<VecDeque<StepAction>>>,
    notify: Arc<Notify>,
    commits: Arc<Mutex<Vec<Vec<Op>>>>,
    submission: pi_durable::harness::submissions::SubmissionHandle,
    _sub: pi_js::Unsubscribe,
}

impl DrivenTool {
    async fn step<F>(&self, action: F) -> Vec<Op>
    where
        F: FnOnce(&ToolExecutionApi) -> BoxFuture<pi_js::Result<()>> + Send + 'static,
    {
        let before = self.commits.lock().expect("lock commits").len();
        self.actions
            .lock()
            .expect("lock actions")
            .push_back(Box::new(move |api| {
                let fut = action(api);
                Box::pin(async move {
                    fut.await?;
                    Ok(true)
                })
            }));
        self.notify.notify_one();
        wait_for(
            || {
                let commits = Arc::clone(&self.commits);
                async move { commits.lock().expect("lock commits").len() > before }
            },
            5_000,
        )
        .await;
        self.commits
            .lock()
            .expect("lock commits")
            .last()
            .expect("last commit")
            .clone()
    }

    async fn finish(self) -> Vec<Vec<Op>> {
        let before = self.commits.lock().expect("lock commits").len();
        self.actions
            .lock()
            .expect("lock actions")
            .push_back(Box::new(|_| Box::pin(async { Ok(false) })));
        self.notify.notify_one();
        self.submission.wait(context()).await.expect("wait");
        self.commits.lock().expect("lock commits")[before..].to_vec()
    }

    fn commits_snapshot(&self) -> Vec<Vec<Op>> {
        self.commits.lock().expect("lock commits").clone()
    }
}

/// Drive one tool call step by step and capture the exact Chord operations of every `pi.live` commit. Each `step()`
/// runs one action inside the tool and returns the operations of the commit it caused.
async fn drive(output_limits: Option<ToolOutputLimits>) -> DrivenTool {
    let setup = chat_setup(None);
    let actions: Arc<Mutex<VecDeque<StepAction>>> = Arc::new(Mutex::new(VecDeque::new()));
    let notify = Arc::new(Notify::new());
    let actions_cb = Arc::clone(&actions);
    let notify_cb = Arc::clone(&notify);
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "drive".to_owned(),
            description: "Driven by the test".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits,
            execute: Arc::new(move |_, api, call_ctx| {
                let actions_cb = Arc::clone(&actions_cb);
                let notify_cb = Arc::clone(&notify_cb);
                Box::pin(async move {
                    loop {
                        let next = actions_cb.lock().expect("lock actions").pop_front();
                        let Some(action) = next else {
                            let abort = call_ctx.abort_signal();
                            tokio::select! {
                                _ = notify_cb.notified() => continue,
                                err = async {
                                    match abort.as_ref() {
                                        Some(sig) => aborted(sig).await,
                                        None => std::future::pending().await,
                                    }
                                } => return err,
                            }
                        };
                        if !action(&api).await? {
                            return Ok(ToolExecutionResult::default());
                        }
                    }
                })
            }),
        }),
        None,
    )
    .expect("add drive");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "drive",
                Default::default(),
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
        .expect("open chat");
    let commits = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let commits_cb = Arc::clone(&commits);
    let sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live" && !change.ops.is_empty() {
                    commits_cb.lock().expect("lock commits").push(change.ops);
                }
            }
        })
        .expect("subscribe commits");
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
    let harness_wait = opened.harness.clone();
    let root_id = opened.root.id();
    wait_for(
        || {
            let harness_wait = harness_wait.clone();
            async move {
                live_state(&harness_wait, root_id)
                    .await
                    .and_then(|s| s.tools)
                    .and_then(|t| t.into_iter().next())
                    .is_some_and(|slot| slot.status == ToolSlotStatus::Running)
            }
        },
        5_000,
    )
    .await;
    DrivenTool {
        harness: opened.harness,
        root_id,
        actions,
        notify,
        commits,
        submission,
        _sub: sub,
    }
}

struct RecordingStorage {
    inner: MemoryStorage,
    live_id: Arc<Mutex<Option<DocumentId>>>,
    written: Arc<Mutex<IndexMap<Seq, &'static str>>>,
}

#[async_trait]
impl Storage for RecordingStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        let live_id = *self.live_id.lock().expect("lock live_id");
        let mut kind: Option<&'static str> = None;
        if let Some(target_id) = live_id {
            for write in &writes {
                if let StorageWrite::DocumentChange { id, content } = write
                    && *id == target_id
                {
                    kind = Some(match content {
                        DocumentContent::Base { .. } => "base",
                        DocumentContent::Delta { .. } => "delta",
                    });
                }
            }
        }
        let seq = self.inner.commit(writes, context).await?;
        if let Some(k) = kind {
            self.written.lock().expect("lock written").insert(seq, k);
        }
        Ok(seq)
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.inner.mint_id().await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor, context).await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry_in_conversation(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.inner.scan_submissions(query, limit, cursor, context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.inner.scan_documents(query, limit, cursor, context).await
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.inner.close(context).await
    }
}

#[tokio::test]
async fn hands_a_generation_over_to_its_tool_round_and_starts_a_tool_with_one_field_write_each() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "noop".to_owned(),
            description: "noop".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| {
                Box::pin(async {
                    Ok(ToolExecutionResult {
                        content: Some(Vec::new()),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add noop");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "noop",
                Default::default(),
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
        .expect("open chat");
    let commits = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let commits_cb = Arc::clone(&commits);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live" && !change.ops.is_empty() {
                    commits_cb.lock().expect("lock commits").push(change.ops);
                }
            }
        })
        .expect("subscribe");
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
    let root_id = opened.root.id();
    let tasks = opened
        .harness
        .session()
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    20,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan_tasks");
    let mut gen_ids: Vec<i64> = tasks
        .items
        .iter()
        .filter(|task| task.kind == "pi.generation")
        .map(|task| task.id.as_i64())
        .collect();
    gen_ids.sort();
    let first_generation = gen_ids[0];
    let second_generation = gen_ids[1];
    let tool = tasks
        .items
        .iter()
        .find(|task| task.kind == "pi.tool")
        .expect("tool task")
        .id
        .as_i64();
    let entries = opened
        .root
        .entries(ConversationEntriesQuery::default(), 10, None, context())
        .await
        .expect("entries");
    let result = entries
        .items
        .iter()
        .find(|entry| entry.kind == "pi.tool-result")
        .expect("tool result")
        .id
        .as_i64();

    let snapshot = commits.lock().expect("lock commits").clone();
    assert_eq!(snapshot.len(), 8);
    // 0: submission
    assert_eq!(snapshot[0].len(), 1);
    match &snapshot[0][0] {
        Op::Set(OpSet(_, path, JsonValue::Object(map))) if path == &vec![seg_s("run")] => {
            assert_eq!(map.get("taskId"), Some(&json!(first_generation)));
            let inputs = map.get("inputs").and_then(|v| v.as_array()).expect("inputs");
            assert_eq!(inputs.len(), 1);
            assert!(inputs[0].is_number());
        }
        other => panic!("Unexpected commit[0]: {other:?}"),
    }
    // 1: request
    assert_eq!(
        snapshot[1],
        vec![op_s(vec![seg_s("generation")], json!({ "attempt": 1 }))]
    );
    // 2: the generation starts its tool round and keeps the run
    assert_eq!(snapshot[2].len(), 2);
    assert!(snapshot[2].contains(&op_d(vec![seg_s("generation")])));
    assert!(snapshot[2].contains(&op_s(
        vec![seg_s("tools")],
        json!([{ "callId": "c1", "name": "noop", "taskId": tool, "status": "pending" }]),
    )));
    // 3: intent
    assert_eq!(
        snapshot[3],
        vec![op_s(vec![seg_s("tools"), seg_i(0), seg_s("status")], json!("running"))]
    );
    // 4: result
    assert_eq!(snapshot[4].len(), 2);
    assert!(snapshot[4].contains(&op_s(vec![seg_s("tools"), seg_i(0), seg_s("status")], json!("done"),)));
    assert!(snapshot[4].contains(&op_s(vec![seg_s("tools"), seg_i(0), seg_s("entry")], json!(result),)));
    // 5: the generation's tools phase hands the run to the next generation
    assert_eq!(snapshot[5].len(), 2);
    assert!(snapshot[5].contains(&op_d(vec![seg_s("tools")])));
    assert!(snapshot[5].contains(&op_s(vec![seg_s("run"), seg_s("taskId")], json!(second_generation))));
    // 6: second generation request
    assert_eq!(
        snapshot[6],
        vec![op_s(vec![seg_s("generation")], json!({ "attempt": 1 }))]
    );
    // 7: the answer ends the run
    assert_eq!(snapshot[7].len(), 2);
    assert!(snapshot[7].contains(&op_d(vec![seg_s("run")])));
    assert!(snapshot[7].contains(&op_d(vec![seg_s("generation")])));

    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn appends_head_output_and_then_only_updates_the_dropped_counts_once_the_window_is_full() {
    let run = drive(Some(ToolOutputLimits {
        max_lines: Some(2),
        max_bytes: None,
        retain: None,
    }))
    .await;
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("one\n") })
        })
        .await,
        vec![op_s(output_path(), json!("one\n"))]
    );
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("two\n") })
        })
        .await,
        vec![op_a(output_path(), "two\n")]
    );
    // The window is full: the retained text stays; only the counts change.
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("three\n") })
        })
        .await,
        vec![
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedBytes")], json!(6)),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedLines")], json!(1)),
        ]
    );
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("four\n") })
        })
        .await,
        vec![
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedBytes")], json!(11)),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedLines")], json!(2)),
        ]
    );
    let harness = run.harness.clone();
    run.finish().await;
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn slides_a_tail_window_as_a_front_trim_plus_an_append() {
    let run = drive(Some(ToolOutputLimits {
        max_lines: Some(3),
        max_bytes: None,
        retain: Some(OutputRetainMode::Tail),
    }))
    .await;
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("line 1\nline 2\nline 3\n") })
        })
        .await,
        vec![op_s(output_path(), json!("line 1\nline 2\nline 3\n"))]
    );
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("line 4\n") })
        })
        .await,
        vec![
            op_t(output_path(), 7),
            op_a(output_path(), "line 4\n"),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedBytes")], json!(7)),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedLines")], json!(1)),
        ]
    );
    // The buffer keeps only the window, and later slides stay minimal and exact.
    assert_eq!(
        run.step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("line 5\nline 6\n") })
        })
        .await,
        vec![
            op_t(output_path(), 14),
            op_a(output_path(), "line 5\nline 6\n"),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedBytes")], json!(21)),
            op_s(vec![seg_s("tools"), seg_i(0), seg_s("droppedLines")], json!(3)),
        ]
    );
    assert_eq!(
        live_state(&run.harness, run.root_id)
            .await
            .and_then(|s| s.tools)
            .and_then(|mut t| t.pop())
            .and_then(|slot| slot.output),
        Some("line 4\nline 5\nline 6\n".to_owned())
    );
    let harness = run.harness.clone();
    run.finish().await;
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn writes_the_whole_window_when_chords_overlap_search_cannot_find_the_shared_part() {
    // A retained window beyond the 64 KiB overlap scan.
    let wide = drive(Some(ToolOutputLimits {
        max_bytes: Some(100 * 1024),
        max_lines: Some(1_000_000),
        retain: Some(OutputRetainMode::Tail),
    }))
    .await;
    let line = |index: usize| -> String { format!("{index:010} {}\n", "x".repeat(989)) };
    let mut text = String::new();
    for index in 0..100 {
        text.push_str(&line(index));
    }
    wide.step(move |api| {
        let api = api.clone();
        Box::pin(async move { api.output(&text) })
    })
    .await;
    let next_chunk = format!("{}{}{}{}", line(100), line(101), line(102), line(103));
    let slid = wide
        .step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.output(&next_chunk) })
        })
        .await;
    let output_verbs: Vec<&str> = slid.iter().filter(|op| is_output(op)).map(op_verb).collect();
    assert_eq!(output_verbs, vec!["s"]);
    let wide_harness = wide.harness.clone();
    wide.finish().await;
    wide_harness.close(context()).await.expect("close");

    // Repetitive output still finds an overlap here; Chord's bounded candidate search can give up on other inputs
    // and then writes one window.
    let repetitive = drive(Some(ToolOutputLimits {
        max_bytes: None,
        max_lines: Some(50),
        retain: Some(OutputRetainMode::Tail),
    }))
    .await;
    let repeated_lines = "y\n".repeat(50);
    repetitive
        .step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.output(&repeated_lines) })
        })
        .await;
    let repeated = repetitive
        .step(|api| {
            let api = api.clone();
            Box::pin(async move { api.output("z\n") })
        })
        .await;
    let rep_outputs: Vec<Op> = repeated.into_iter().filter(is_output).collect();
    assert_eq!(rep_outputs, vec![op_t(output_path(), 2), op_a(output_path(), "z\n")]);
    let rep_harness = repetitive.harness.clone();
    repetitive.finish().await;
    rep_harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn diffs_details_leaf_by_leaf_and_appends_diagnostics() {
    let run = drive(None).await;
    let details_path = || vec![seg_s("tools"), seg_i(0), seg_s("details")];
    assert_eq!(
        run.step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.details(&json!({ "step": 1, "log": "a" }), context()).await })
        })
        .await,
        vec![op_s(details_path(), json!({ "step": 1, "log": "a" }))]
    );
    let step2 = run
        .step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.details(&json!({ "step": 2, "log": "ab" }), context()).await })
        })
        .await;
    let mut expected_step = details_path();
    expected_step.push(seg_s("step"));
    let mut expected_log = details_path();
    expected_log.push(seg_s("log"));
    assert!(step2.contains(&op_s(expected_step, json!(2))));
    assert!(step2.contains(&op_a(expected_log.clone(), "b")));

    assert_eq!(
        run.step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.details(&json!({ "step": 2 }), context()).await })
        })
        .await,
        vec![op_d(expected_log)]
    );

    let diagnostics_path = || vec![seg_s("tools"), seg_i(0), seg_s("diagnostics")];
    let first = ToolDiagnostic {
        severity: DiagnosticSeverity::Info,
        message: "first".to_owned(),
        code: None,
    };
    let second = ToolDiagnostic {
        severity: DiagnosticSeverity::Warn,
        message: "second".to_owned(),
        code: None,
    };
    let first_json = serde_json::to_value(&first).expect("first json");
    let second_json = serde_json::to_value(&second).expect("second json");
    let first_cb = first.clone();
    assert_eq!(
        run.step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.diagnostic(first_cb) })
        })
        .await,
        vec![op_s(diagnostics_path(), JsonValue::Array(vec![first_json]))]
    );
    let second_cb = second.clone();
    assert_eq!(
        run.step(move |api| {
            let api = api.clone();
            Box::pin(async move { api.diagnostic(second_cb) })
        })
        .await,
        vec![op_p(diagnostics_path(), 1, 0, vec![second_json])]
    );

    // Settlement moves everything into the result entry and keeps the slot small.
    let harness = run.harness.clone();
    let settled_commits = run.finish().await;
    let settled = &settled_commits[0];
    assert!(settled.contains(&op_s(vec![seg_s("tools"), seg_i(0), seg_s("status")], json!("done"),)));
    assert!(settled.contains(&op_d(details_path())));
    assert!(settled.contains(&op_d(diagnostics_path())));
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn streams_partial_text_as_appends() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        tokens_per_second: Some(400.0),
        token_size: Some(FauxTokenSize {
            min: Some(4),
            max: Some(4),
        }),
        ..Default::default()
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(&"word ".repeat(250)))),
            None,
        ))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let commits = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let commits_cb = Arc::clone(&commits);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live" && !change.ops.is_empty() {
                    commits_cb.lock().expect("lock commits").push(change.ops);
                }
            }
        })
        .expect("subscribe");
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

    let snapshot = commits.lock().expect("lock commits").clone();
    let partials = &snapshot[2..snapshot.len() - 1];
    assert!(partials.len() > 2);
    assert_eq!(partials[0].len(), 1);
    match &partials[0][0] {
        Op::Set(OpSet(_, path, JsonValue::Object(_))) if path == &vec![seg_s("generation"), seg_s("message")] => {}
        other => panic!("Unexpected initial partial commit: {other:?}"),
    }
    let expected_text_path = vec![
        seg_s("generation"),
        seg_s("message"),
        seg_s("content"),
        seg_i(0),
        seg_s("text"),
    ];
    for ops in &partials[1..] {
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Op::Append(OpAppend(_, path, _)) if path == &expected_text_path => {}
            other => panic!("Expected text append op, got {other:?}"),
        }
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stores_a_complete_base_exactly_in_the_commits_where_nothing_runs() {
    let written = Arc::new(Mutex::new(IndexMap::<Seq, &'static str>::new()));
    let live_id = Arc::new(Mutex::new(None::<DocumentId>));
    let storage = Arc::new(RecordingStorage {
        inner: MemoryStorage::new(),
        live_id: Arc::clone(&live_id),
        written: Arc::clone(&written),
    });

    let setup = chat_setup(None);
    for name in ["first", "second"] {
        let name_owned = name.to_owned();
        add_tool(
            &setup.registry,
            define_tool(ToolRegistration {
                name: name.to_owned(),
                description: name.to_owned(),
                parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
                replay: None,
                execution_mode: None,
                prepare_arguments: None,
                output_limits: None,
                execute: Arc::new(move |_, api, _| {
                    let name_owned = name_owned.clone();
                    Box::pin(async move {
                        api.output(&format!("{name_owned} output\n"))?;
                        pi_js::time::sleep(150, None).await?;
                        api.output(&format!("{name_owned} more\n"))?;
                        Ok(ToolExecutionResult::default())
                    })
                }),
            }),
            None,
        )
        .expect("add tool");
    }
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::ToolCall(faux_tool_call(
                    "first",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("a".to_owned()),
                    }),
                )),
                FauxContentBlock::ToolCall(faux_tool_call(
                    "second",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("b".to_owned()),
                    }),
                )),
            ]),
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
    setup.settings.update(|s| {
        s.tool_execution = Some(ToolExecutionMode::Sequential);
    });
    let opened = open_chat(storage, &setup, OpenChatEnv::None).await.expect("open chat");
    let values = Arc::new(Mutex::new(IndexMap::<Seq, LiveState>::new()));
    let values_cb = Arc::clone(&values);
    let live_id_cb = Arc::clone(&live_id);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind != "pi.live" {
                    continue;
                }
                *live_id_cb.lock().expect("lock live_id") = Some(change.record.id);
                if let Some(val) = change.value
                    && let Ok(parsed) = serde_json::from_value::<LiveState>(JsonValue::Object(val))
                {
                    values_cb.lock().expect("lock values").insert(publication.seq, parsed);
                }
            }
        })
        .expect("subscribe");
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
    flush().await;

    let nothing_runs = |value: &LiveState| {
        value.generation.is_none()
            && !value
                .tools
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .any(|slot| slot.status == ToolSlotStatus::Running)
    };
    let written_guard = written.lock().expect("lock written");
    let values_guard = values.lock().expect("lock values");
    let mut kinds = Vec::new();
    for (seq, kind) in written_guard.iter() {
        let value = values_guard.get(seq).expect("recorded commit value");
        assert_eq!(*kind == "base", nothing_runs(value));
        kinds.push(*kind);
    }
    // Bases at the handover, after each sequential tool, after the round, and when the run ends.
    assert!(kinds.iter().filter(|&&k| k == "base").count() >= 5);
    assert!(kinds.contains(&"delta"));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn starts_calls_the_request_did_not_offer_as_done_and_marks_a_faulted_tool_s_slot_done_without_an_entry() {
    let storage = Arc::new(ControlledStorage::new());
    let setup = chat_setup(None);
    let storage_cb = Arc::clone(&storage);
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "bad".to_owned(),
            description: "bad".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_, _, _| {
                let storage_cb = Arc::clone(&storage_cb);
                Box::pin(async move {
                    // Reject the result commit so the scheduler faults the tool task.
                    storage_cb.fail_next_commit(pi_durable::errors::StorageRejected::new("not json").into_error());
                    Ok(ToolExecutionResult {
                        content: Some(Vec::new()),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add bad");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::ToolCall(faux_tool_call(
                    "ghost",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("g".to_owned()),
                    }),
                )),
                FauxContentBlock::ToolCall(faux_tool_call(
                    "bad",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("b".to_owned()),
                    }),
                )),
            ]),
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
    let opened = open_chat(storage, &setup, OpenChatEnv::None).await.expect("open chat");
    let commits = Arc::new(Mutex::new(Vec::<Vec<Op>>::new()));
    let commits_cb = Arc::clone(&commits);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live" && !change.ops.is_empty() {
                    commits_cb.lock().expect("lock commits").push(change.ops);
                }
            }
        })
        .expect("subscribe");
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
    let entries = opened
        .root
        .entries(ConversationEntriesQuery::default(), 10, None, context())
        .await
        .expect("entries");
    let ghost_result = entries
        .items
        .iter()
        .find(|entry| entry.kind == "pi.tool-result")
        .expect("ghost tool result")
        .id
        .as_i64();
    let snapshot = commits.lock().expect("lock commits").clone();
    let handover = snapshot
        .iter()
        .find(|ops| {
            ops.iter()
                .any(|op| matches!(op, Op::Set(OpSet(_, path, _)) if path == &vec![seg_s("tools")]))
        })
        .expect("handover commit");
    let tools_op = handover
        .iter()
        .find_map(|op| match op {
            Op::Set(OpSet(_, path, JsonValue::Array(items))) if path == &vec![seg_s("tools")] => Some(items),
            _ => None,
        })
        .expect("tools set op");
    assert_eq!(tools_op.len(), 2);
    assert_eq!(
        tools_op[0],
        json!({ "callId": "g", "name": "ghost", "status": "done", "entry": ghost_result })
    );
    let bad_slot = tools_op[1].as_object().expect("bad slot object");
    assert_eq!(bad_slot.get("callId"), Some(&json!("b")));
    assert_eq!(bad_slot.get("name"), Some(&json!("bad")));
    assert_eq!(bad_slot.get("status"), Some(&json!("pending")));
    assert!(bad_slot.get("taskId").is_some_and(|v| v.is_number()));

    // The fault cleanup writes only the status.
    assert!(snapshot.contains(&vec![op_s(
        vec![seg_s("tools"), seg_i(1), seg_s("status")],
        json!("done"),
    )]));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn commits_the_tool_calling_answer_its_tool_tasks_the_generations_wait_and_the_tool_round_in_one_commit() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "noop".to_owned(),
            description: "noop".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| {
                Box::pin(async {
                    Ok(ToolExecutionResult {
                        content: Some(Vec::new()),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add noop");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::ToolCall(faux_tool_call(
                    "noop",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("a".to_owned()),
                    }),
                )),
                FauxContentBlock::ToolCall(faux_tool_call(
                    "noop",
                    Default::default(),
                    Some(FauxToolCallOptions {
                        id: Some("b".to_owned()),
                    }),
                )),
            ]),
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
        .expect("open chat");
    let handover = Arc::new(Mutex::new(None::<CommitPublication>));
    let handover_cb = Arc::clone(&handover);
    let _sub = opened
        .harness
        .subscribe_commits(move |publication, _| {
            for change in document_changes(publication) {
                if change.record.kind == "pi.live"
                    && let Some(val) = change.value
                    && let Ok(parsed) = serde_json::from_value::<LiveState>(JsonValue::Object(val))
                    && parsed.tools.as_ref().is_some_and(|t| t.len() == 2)
                {
                    let mut guard = handover_cb.lock().expect("lock handover");
                    if guard.is_none() {
                        *guard = Some(publication.clone());
                    }
                }
            }
        })
        .expect("subscribe");
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
    let pub_record = handover.lock().expect("lock handover").take().expect("handover");
    let mut kinds: Vec<String> = pub_record
        .changes
        .iter()
        .filter_map(|change| match change {
            CommitChange::Table(TableCommitChange::Entry(entry)) => Some(entry.kind.clone()),
            CommitChange::Table(TableCommitChange::Task(task)) => Some(task.kind.clone()),
            _ => None,
        })
        .collect();
    kinds.sort();
    assert_eq!(kinds, vec!["pi.assistant", "pi.generation", "pi.tool", "pi.tool"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn writes_an_aborted_tool_s_slot_with_field_level_ops() {
    let run = drive(None).await;
    run.step(|api| {
        let api = api.clone();
        Box::pin(async move { api.output("partial\n") })
    })
    .await;
    run.step(|api| {
        let api = api.clone();
        Box::pin(async move { api.details(&json!({ "n": 1 }), context()).await })
    })
    .await;
    let task_id = live_state(&run.harness, run.root_id)
        .await
        .and_then(|s| s.tools)
        .and_then(|mut t| t.pop())
        .and_then(|slot| slot.task_id)
        .expect("task_id");
    let before = run.commits_snapshot().len();
    run.harness
        .abort_task(task_id.erased(), context())
        .await
        .expect("abort");
    let is_abort_commit = |ops: &[Op]| {
        ops.iter().any(|op| {
            matches!(
                op,
                Op::Set(OpSet(_, path, val))
                    if path == &vec![seg_s("tools"), seg_i(0), seg_s("status")] && val == &json!("done")
            )
        })
    };
    wait_for(
        || {
            let commits = Arc::clone(&run.commits);
            async move {
                commits.lock().expect("lock commits")[before..]
                    .iter()
                    .any(|ops| is_abort_commit(ops))
            }
        },
        5_000,
    )
    .await;
    let snapshot = run.commits_snapshot();
    let abort_commit = snapshot[before..]
        .iter()
        .find(|ops| is_abort_commit(ops))
        .expect("abort commit");
    assert_eq!(abort_commit.len(), 4);
    assert!(abort_commit.contains(&op_s(vec![seg_s("tools"), seg_i(0), seg_s("status")], json!("done"),)));
    assert!(abort_commit.iter().any(|op| {
        matches!(
            op,
            Op::Set(OpSet(_, path, val))
                if path == &vec![seg_s("tools"), seg_i(0), seg_s("entry")] && val.is_number()
        )
    }));
    assert!(abort_commit.contains(&op_d(output_path())));
    assert!(abort_commit.contains(&op_d(vec![seg_s("tools"), seg_i(0), seg_s("details")])));
    run.harness.close(context()).await.expect("close");
}

#[test]
fn keeps_a_complete_base_exactly_while_nothing_runs() {
    let base = |value: LiveState| -> bool {
        let obj = serde_json::to_value(value)
            .expect("serialize LiveState")
            .as_object()
            .expect("object")
            .clone();
        LIVE_DOC
            .definition
            .checkpoint_when(
                &obj,
                &[],
                CheckpointInfo {
                    deltas_since_base: 1000,
                },
            )
            .expect("checkpoint_when")
    };
    let slot = |status: ToolSlotStatus| ToolSlot {
        call_id: "c".to_owned(),
        name: "n".to_owned(),
        task_id: None,
        status,
        output: None,
        dropped_bytes: None,
        dropped_lines: None,
        details: None,
        diagnostics: None,
        entry: None,
    };
    let run = LiveRun {
        task_id: TaskId::new_unchecked(1),
        inputs: Vec::new(),
    };
    assert!(base(LiveState::default()));
    assert!(!base(LiveState {
        run: Some(run.clone()),
        generation: Some(LiveGeneration {
            attempt: 1,
            message: None,
            retry: None,
            deferred: None,
        }),
        ..Default::default()
    }));
    assert!(base(LiveState {
        run: Some(run.clone()),
        tools: Some(vec![slot(ToolSlotStatus::Pending), slot(ToolSlotStatus::Done)]),
        ..Default::default()
    }));
    assert!(!base(LiveState {
        run: Some(run.clone()),
        tools: Some(vec![slot(ToolSlotStatus::Done), slot(ToolSlotStatus::Running)]),
        ..Default::default()
    }));
    assert!(base(LiveState {
        run: Some(run),
        ..Default::default()
    }));
}

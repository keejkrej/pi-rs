//! Port of packages/durable/test/harness-tools-recovery.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{AssistantMessage, Message, StopReason, ToolResultMessage, UserContentBlock};
use pi_chord::Context;
use pi_durable::entries::TOOL_RESULT_ENTRY;
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::define::define_tool;
use pi_durable::harness::generation::{GENERATION_TASK, GenerationHooks};
use pi_durable::harness::harness::Harness;
use pi_durable::harness::live::LIVE_DOC;
use pi_durable::harness::scheduler::AbortTaskResult;
use pi_durable::harness::tool::{TOOL_TASK, ToolHooks, ToolTaskResult};
use pi_durable::harness::types::{
    AgentChange, AgentExtensionsChange, DocumentReaderExt, FieldChange, SubmissionDraft, ToolExecutionApi,
    ToolExecutionResult, ToolRegistration, ToolReplayMode, UserInput,
};
use pi_durable::storage::sqlite::node::{NodeSqliteStorage, NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::{
    ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId, DocumentPoint,
    DocumentQuery, DocumentRecord, EntryId, EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject,
    JsonValue, Page, Seq, Storage, StorageWrite, StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord,
    SubmissionStatus, TaskId, TaskOutcome, TaskQuery, TaskState,
};
use pi_js::vendor::typebox::{Schema, builder as t};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{ChatSetup, OpenChatEnv, all_entries, chat_setup, open_chat, wait_for};
use support::harness_support::{add_hooks, add_tool};
use support::session_support::context;
use support::task_support::{aborted, deferred};
use tempfile::TempDir;

async fn sqlite_path() -> (TempDir, String) {
    let dir = TempDir::new().expect("create tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

async fn open_sqlite_chat(path: &str, setup: &ChatSetup, env: OpenChatEnv) -> support::chat_support::OpenedChat {
    let storage: Arc<NodeSqliteStorage> = Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    );
    let opened = open_chat(storage as Arc<dyn Storage>, setup, env)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    opened
}

fn tool<F, Fut>(name: &str, execute: F, replay: Option<ToolReplayMode>) -> ToolRegistration
where
    F: Fn(JsonValue, ToolExecutionApi, Context) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = pi_js::Result<ToolExecutionResult>> + Send + 'static,
{
    define_tool(ToolRegistration {
        name: name.to_owned(),
        description: name.to_owned(),
        parameters: t::object([] as [(&str, Schema); 0]).to_value(),
        replay,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |args, api, ctx| Box::pin(execute(args, api, ctx))),
    })
}

fn call(name: &str, id: &str) -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
            name,
            JsonObject::new(),
            Some(FauxToolCallOptions {
                id: Some(id.to_owned()),
            }),
        ))),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    )
}

fn done_msg() -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
        None,
    )
}

fn results(entries: &[EntryRecord]) -> Vec<ToolResultMessage> {
    entries
        .iter()
        .filter(|entry| TOOL_RESULT_ENTRY.matches(entry))
        .filter_map(|entry| match entry.model.as_ref()?.first() {
            Some(Message::ToolResult(tr)) => Some(tr.clone()),
            _ => None,
        })
        .collect()
}

fn text_of_result(message: &ToolResultMessage) -> String {
    message
        .content
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(t) => t.text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

async fn tool_task_id(harness: &Harness, root_id: pi_durable::types::ConversationId) -> TaskId {
    let found = Arc::new(Mutex::new(None::<TaskId>));
    let found_cb = Arc::clone(&found);
    wait_for(
        move || {
            let harness = harness.clone();
            let found_cb = Arc::clone(&found_cb);
            async move {
                let id = harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.tools)
                    .and_then(|mut v| v.pop())
                    .and_then(|slot| slot.task_id);
                if let Some(tid) = id {
                    *found_cb.lock().unwrap() = Some(tid);
                    true
                } else {
                    false
                }
            }
        },
        5_000,
    )
    .await;
    found.lock().unwrap().expect("tool task id")
}

struct BlockingTool {
    registration: ToolRegistration,
    started: support::task_support::Deferred<()>,
    runs: Arc<AtomicUsize>,
}

fn blocking_tool(name: &str, replay: Option<ToolReplayMode>) -> BlockingTool {
    let started = deferred();
    let runs = Arc::new(AtomicUsize::new(0));
    let started_cb = started.clone();
    let runs_cb = Arc::clone(&runs);
    let registration = tool(
        name,
        move |_, api, call_ctx| {
            let started_cb = started_cb.clone();
            let runs_cb = Arc::clone(&runs_cb);
            async move {
                let run = runs_cb.fetch_add(1, Ordering::SeqCst) + 1;
                api.output(&format!("run {run}\n")).expect("output");
                api.details(&json!({ "run": run }), call_ctx.clone()).await?;
                if run <= 1 {
                    started_cb.resolve(());
                    let signal = call_ctx.abort_signal().expect("signal");
                    aborted::<()>(&signal).await?;
                }
                Ok(ToolExecutionResult::default())
            }
        },
        replay,
    );
    BlockingTool {
        registration,
        started,
        runs,
    }
}

#[tokio::test]
async fn answers_an_unsafe_tool_interrupted_after_intent_with_its_durable_partial_output() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let blocking = blocking_tool("work", None);
    add_tool(&setup.registry, blocking.registration, None).expect("add work");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(call("work", "c1")),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let id = opened
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
        .id();
    blocking.started.wait().await.expect("started");
    let task_id = tool_task_id(&opened.harness, opened.root.id()).await;
    opened.harness.close(context()).await.expect("close");

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let rec = opened
        .harness
        .get_task(task_id.cast::<JsonValue>(), context())
        .await
        .expect("get task")
        .expect("task present");
    assert_eq!(
        rec.state,
        TaskState::Running {
            checkpoint: json!({
                "phase": "execute",
                "arguments": {},
                "replay": "unsafe",
            }),
            sleep_until: None,
        }
    );
    let settled = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("sub some")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    assert_eq!(blocking.runs.load(Ordering::SeqCst), 1);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let r = &results(&entries)[0];
    assert!(r.is_error);
    assert_eq!(r.details, Some(json!({ "run": 1 })));
    assert_eq!(
        text_of_result(r),
        "run 1\n|<harness>\n[error] Tool work was interrupted and may have partially run\n</harness>"
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(pi_durable::harness::live::LiveState::default())
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reruns_a_tool_only_when_both_the_stored_and_the_current_replay_policy_are_safe() {
    let cases = [
        (ToolReplayMode::Safe, ToolReplayMode::Safe, true),
        (ToolReplayMode::Safe, ToolReplayMode::Unsafe, false),
        (ToolReplayMode::Unsafe, ToolReplayMode::Safe, false),
    ];
    for (stored, current, reruns) in cases {
        let (_dir, path) = sqlite_path().await;
        let setup = chat_setup(None);
        let blocking = blocking_tool("work", Some(stored));
        let mut updated = blocking.registration.clone();
        updated.replay = Some(current);
        let registration = add_tool(&setup.registry, blocking.registration, None).expect("add");
        setup.faux.set_responses(vec![
            FauxResponseStep::Message(call("work", "c1")),
            FauxResponseStep::Message(done_msg()),
        ]);
        let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
        let id = opened
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
            .id();
        blocking.started.wait().await.expect("started");
        opened.harness.close(context()).await.expect("close");

        registration.dispose();
        add_tool(&setup.registry, updated, None).expect("add updated");
        let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
        let settled = opened
            .harness
            .submission(id, context())
            .await
            .expect("sub")
            .expect("some")
            .wait(context())
            .await
            .expect("wait");
        assert_eq!(settled.record.status(), SubmissionStatus::Done);
        let entries = all_entries(&opened.root, None).await.expect("entries");
        let r = &results(&entries)[0];
        assert_eq!(blocking.runs.load(Ordering::SeqCst), if reruns { 2 } else { 1 });
        assert_eq!(r.is_error, !reruns);
        if reruns {
            assert_eq!(text_of_result(r), "run 2\n");
        }
        opened.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn reruns_a_safe_tool_with_the_environment_of_the_conversations_cwd_at_rerun_and_not_once_it_is_deselected() {
    for change in ["cwd", "deselect"] {
        let (_dir, path) = sqlite_path().await;
        let setup = chat_setup(None);
        let cwds = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let started = deferred();
        let cwds_cb = Arc::clone(&cwds);
        let started_cb = started.clone();

        let work = tool(
            "work",
            move |_, api, call_ctx| {
                let cwds_cb = Arc::clone(&cwds_cb);
                let started_cb = started_cb.clone();
                async move {
                    let count = {
                        let mut g = cwds_cb.lock().unwrap();
                        g.push(api.env().map(|e| e.cwd().to_owned()));
                        g.len()
                    };
                    if count == 1 {
                        started_cb.resolve(());
                        let signal = call_ctx.abort_signal().expect("signal");
                        aborted::<()>(&signal).await?;
                    }
                    Ok(ToolExecutionResult::default())
                }
            },
            Some(ToolReplayMode::Safe),
        );
        add_tool(&setup.registry, work, None).expect("add work");
        setup.faux.set_responses(vec![
            FauxResponseStep::Message(call("work", "c1")),
            FauxResponseStep::Message(done_msg()),
        ]);
        let env = OpenChatEnv::Factory(Arc::new(|target, _| {
            Box::pin(async move {
                let env: Arc<dyn ExecutionEnv> =
                    Arc::new(NodeExecutionEnv::new(target.cwd.unwrap_or_else(|| "/".to_owned())));
                Ok(Some(env))
            })
        }));

        let opened = open_sqlite_chat(&path, &setup, env.clone()).await;
        opened
            .root
            .configure(
                AgentChange {
                    cwd: FieldChange::Set("/one".to_owned()),
                    ..Default::default()
                },
                context(),
            )
            .await
            .expect("configure /one");
        let id = opened
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
            .id();
        started.wait().await.expect("started");
        let reconfigure = if change == "cwd" {
            AgentChange {
                cwd: FieldChange::Set("/two".to_owned()),
                ..Default::default()
            }
        } else {
            AgentChange {
                extensions: FieldChange::Set(AgentExtensionsChange::replace(&[])),
                ..Default::default()
            }
        };
        opened
            .root
            .configure(reconfigure, context())
            .await
            .expect("reconfigure");
        opened.harness.close(context()).await.expect("close");

        let opened = open_sqlite_chat(&path, &setup, env).await;
        let settled = opened
            .harness
            .submission(id, context())
            .await
            .expect("sub")
            .expect("some")
            .wait(context())
            .await
            .expect("wait");
        assert_eq!(settled.record.status(), SubmissionStatus::Done);
        let entries = all_entries(&opened.root, None).await.expect("entries");
        let r = &results(&entries)[0];
        if change == "cwd" {
            assert_eq!(
                *cwds.lock().unwrap(),
                vec![Some("/one".to_owned()), Some("/two".to_owned())]
            );
            assert!(!r.is_error);
        } else {
            assert_eq!(*cwds.lock().unwrap(), vec![Some("/one".to_owned())]);
            assert!(text_of_result(r).contains("was interrupted"));
        }
        opened.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn reruns_before_tool_when_interrupted_before_intent_and_executes_once() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let runs = Arc::new(AtomicUsize::new(0));
    let runs_cb = Arc::clone(&runs);
    add_tool(
        &setup.registry,
        tool(
            "work",
            move |_, _, _| {
                let runs_cb = Arc::clone(&runs_cb);
                async move {
                    runs_cb.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolExecutionResult {
                        content: Some(vec![]),
                        ..Default::default()
                    })
                }
            },
            None,
        ),
        None,
    )
    .expect("add work");

    let reached = deferred();
    let asked = Arc::new(AtomicUsize::new(0));
    let decisions = Arc::new(Mutex::new(Vec::<String>::new()));

    let reached_cb = reached.clone();
    let asked_cb = Arc::clone(&asked);
    let decisions_cb = Arc::clone(&decisions);
    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            before_tool: Some(Arc::new(move |_, api, call_ctx| {
                let reached_cb = reached_cb.clone();
                let asked_cb = Arc::clone(&asked_cb);
                let decisions_cb = Arc::clone(&decisions_cb);
                Box::pin(async move {
                    let count = asked_cb.fetch_add(1, Ordering::SeqCst) + 1;
                    let dec: String = api
                        .memo_set("test:decision", &format!("attempt {count}"), call_ctx.clone())
                        .await?;
                    decisions_cb.lock().unwrap().push(dec);
                    if count == 1 {
                        reached_cb.resolve(());
                        let signal = call_ctx.abort_signal().expect("signal");
                        aborted::<()>(&signal).await?;
                    }
                    Ok(None)
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add before_tool");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(call("work", "c1")),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let id = opened
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
        .id();
    reached.wait().await.expect("reached");
    opened.harness.close(context()).await.expect("close");

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let settled = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("some")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(*decisions.lock().unwrap(), vec!["attempt 1", "attempt 1"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reruns_the_generation_tools_phase_interrupted_before_its_commit() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool(
            "work",
            |_, _, _| async {
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            },
            None,
        ),
        None,
    )
    .expect("add work");

    let reached = deferred();
    let observed = Arc::new(AtomicUsize::new(0));
    let reached_cb = reached.clone();
    let observed_cb = Arc::clone(&observed);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_tools: Some(Arc::new(move |_, _, _, call_ctx| {
                let reached_cb = reached_cb.clone();
                let observed_cb = Arc::clone(&observed_cb);
                Box::pin(async move {
                    if observed_cb.fetch_add(1, Ordering::SeqCst) == 0 {
                        reached_cb.resolve(());
                        let signal = call_ctx.abort_signal().expect("signal");
                        aborted::<()>(&signal).await?;
                    }
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add after_tools");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(call("work", "c1")),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let id = opened
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
        .id();
    reached.wait().await.expect("reached");
    opened.harness.close(context()).await.expect("close");

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    let settled = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("some")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    let kinds: Vec<String> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        vec!["pi.user", "pi.system", "pi.assistant", "pi.tool-result", "pi.assistant"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_an_aborted_tool_with_its_partial_output_and_continues_the_run() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let blocking = blocking_tool("work", None);
    add_tool(&setup.registry, blocking.registration, None).expect("add work");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(call("work", "c1")),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
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
    blocking.started.wait().await.expect("started");
    let task_id = tool_task_id(&opened.harness, opened.root.id()).await;
    assert_eq!(
        opened.harness.abort_task(task_id, context()).await.expect("abort"),
        AbortTaskResult::Marked
    );
    let settled_task = opened
        .harness
        .wait_for_task(task_id.cast::<ToolTaskResult>(), context())
        .await
        .expect("wait task");
    match settled_task.outcome {
        TaskOutcome::Aborted { result: Some(_res), .. } => {}
        other => panic!("unexpected outcome: {other:?}"),
    }
    let settled = submission.wait(context()).await.expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        text_of_result(&results(&entries)[0]),
        "run 1\n|<harness>\n[error] Tool work was aborted\n</harness>"
    );
    opened.harness.close(context()).await.expect("close");
}

struct FailingToolResultStorage {
    inner: NodeSqliteStorage,
}

#[async_trait]
impl Storage for FailingToolResultStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        let has_tool_result = writes.iter().any(|w| match w {
            StorageWrite::Entry { value } => value.kind == "pi.tool-result",
            _ => false,
        });
        if has_tool_result {
            return Err(pi_js::Error::js("TypeError", "non-strict json in tool result"));
        }
        self.inner.commit(writes, context).await
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
async fn lets_context_derivation_answer_a_faulted_tool_and_continues_the_run() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    // Rejecting pi.tool-result writes during settle faults the tool task cleanly.
    add_tool(
        &setup.registry,
        tool(
            "bad",
            |_, _, _| async {
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            },
            None,
        ),
        None,
    )
    .expect("add bad");

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_cb = Arc::clone(&requests);
    let check_step = FauxResponseStep::Factory(Arc::new(move |request, _, _, _| {
        let requests_cb = Arc::clone(&requests_cb);
        Box::pin(async move {
            let found = request.messages.iter().find_map(|m| match m {
                Message::ToolResult(tr) => Some(text_of_result(tr)),
                _ => None,
            });
            requests_cb
                .lock()
                .unwrap()
                .push(found.unwrap_or_else(|| "none".to_owned()));
            Ok(done_msg())
        })
    }));
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(call("bad", "c1")), check_step]);
    let sqlite = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open sqlite");
    let storage: Arc<dyn Storage> = Arc::new(FailingToolResultStorage { inner: sqlite });
    let opened = open_chat(storage, &setup, OpenChatEnv::None).await.expect("open");
    opened.harness.resume().expect("resume");
    let settled = opened
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
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert!(results(&entries).is_empty());
    assert_eq!(
        *requests.lock().unwrap(),
        vec!["Tool result unavailable: history ends before this call completed."]
    );
    let conv_id = opened.root.id();
    let tasks = opened
        .harness
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(conv_id),
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
        .expect("scan");
    let faulted = tasks.items.iter().find(|t| t.kind == "pi.tool").expect("faulted tool");
    assert!(matches!(
        faulted.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Faulted { .. }
        }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_a_real_bash_command_interrupted_by_close_and_reopen_then_finishes_the_run() {
    let (dir, path) = sqlite_path().await;
    let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.path().to_string_lossy().into_owned()));
    let setup = chat_setup(None);
    add_tool(&setup.registry, pi_durable::tools::create_bash_tool(None), None).expect("add bash");
    let mut call_args = JsonObject::new();
    call_args.insert("command".to_owned(), json!("echo started; sleep 30"));
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "bash",
                call_args,
                Some(FauxToolCallOptions {
                    id: Some("b".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(done_msg()),
    ]);

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::One(Arc::clone(&env))).await;
    let id = opened
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
        .id();
    let root_id = opened.root.id();
    let harness_c = opened.harness.clone();
    wait_for(
        move || {
            let harness = harness_c.clone();
            async move {
                harness
                    .snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .expect("live")
                    .and_then(|s| s.tools)
                    .and_then(|v| v.first().and_then(|slot| slot.output.clone()))
                    == Some("started\n".to_owned())
            }
        },
        5_000,
    )
    .await;
    opened.harness.close(context()).await.expect("close");

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::One(env)).await;
    let settled = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("some")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        text_of_result(&results(&entries)[0]),
        "started\n|<harness>\n[error] Tool bash was interrupted and may have partially run\n</harness>"
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn clears_the_interrupted_attempts_progress_before_a_safe_rerun() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let started0 = deferred();
    let started1 = deferred();
    let runs = Arc::new(AtomicUsize::new(0));
    let s0_cb = started0.clone();
    let s1_cb = started1.clone();
    let runs_cb = Arc::clone(&runs);

    add_tool(
        &setup.registry,
        tool(
            "work",
            move |_, api, call_ctx| {
                let s0_cb = s0_cb.clone();
                let s1_cb = s1_cb.clone();
                let runs_cb = Arc::clone(&runs_cb);
                async move {
                    let run = runs_cb.fetch_add(1, Ordering::SeqCst);
                    if run == 0 {
                        api.diagnostic(ToolDiagnostic {
                            severity: pi_durable::harness::types::DiagnosticSeverity::Info,
                            message: "first a".to_owned(),
                            code: None,
                        })
                        .expect("d1");
                        api.diagnostic(ToolDiagnostic {
                            severity: pi_durable::harness::types::DiagnosticSeverity::Info,
                            message: "first b".to_owned(),
                            code: None,
                        })
                        .expect("d2");
                        api.details(&json!({ "run": 1, "extra": true }), call_ctx.clone())
                            .await?;
                        s0_cb.resolve(());
                    } else {
                        api.diagnostic(ToolDiagnostic {
                            severity: pi_durable::harness::types::DiagnosticSeverity::Info,
                            message: "second".to_owned(),
                            code: None,
                        })
                        .expect("d3");
                        api.details(&json!({ "run": 2 }), call_ctx.clone()).await?;
                        s1_cb.resolve(());
                    }
                    let signal = call_ctx.abort_signal().expect("signal");
                    aborted::<()>(&signal).await?;
                    Ok(ToolExecutionResult::default())
                }
            },
            Some(ToolReplayMode::Safe),
        ),
        None,
    )
    .expect("add work");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(call("work", "c1")),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
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
        .expect("submit");
    started0.wait().await.expect("started 0");
    opened.harness.close(context()).await.expect("close");

    let opened = open_sqlite_chat(&path, &setup, OpenChatEnv::None).await;
    started1.wait().await.expect("started 1");
    let slot = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("some")
        .tools
        .expect("tools")
        .into_iter()
        .next()
        .expect("slot");
    assert_eq!(
        slot.diagnostics,
        Some(vec![ToolDiagnostic {
            severity: pi_durable::harness::types::DiagnosticSeverity::Info,
            message: "second".to_owned(),
            code: None,
        }])
    );
    assert_eq!(slot.details, Some(json!({ "run": 2 })));
    opened.harness.close(context()).await.expect("close");
}

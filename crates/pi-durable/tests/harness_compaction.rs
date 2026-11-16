//! Port of packages/durable/test/harness-compaction.test.ts

mod support;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxModelDefinition,
    FauxResponseFactory, FauxResponseStep, FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message,
    faux_text, faux_thinking, faux_tool_call,
};
use pi_ai::types::{
    AssistantMessage, CacheRetention, Message, SimpleStreamOptions, StopReason, SystemMessage, SystemMessageContent,
    ToolResultMessage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_ai::{TextContent, Type};
use pi_chord::Context;
use pi_durable::errors::StorageRejected;
use pi_durable::harness::compaction::{COMPACTION_TASK, CompactionInput, select_cut, serialize_conversation};
use pi_durable::harness::context::order_tool_results;
use pi_durable::harness::define::define_tool;
use pi_durable::harness::events::AgentEvent;
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::live::{CompactionStatus, LIVE_DOC, LiveState};
use pi_durable::harness::types::{
    AgentChange, AgentToolsChange, BeforeCompactDecision, CompactionHooks, CompactionPolicy, CompactionReason,
    CompactionResult, ContextView, ConversationCreateOptions, FieldChange, ModelRef, PartialCompactionPolicy,
    PartialRetryPolicy, StreamDeferredOption, SubmissionDraft, TaskBlockedReason, TaskInspectionState,
    ToolExecutionResult, UserInput,
};
use pi_durable::harness::usage::USAGE_DOC;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{
    ContextEdit, ConversationOwnership, EntryDraft, EntryDraftHead, EntryId, EntryRecord, InputSubmissionState,
    NextTaskState, Storage, SubmissionId, SubmissionState, TaskId, TaskOptions, TaskOutcome, TaskOwnership, TaskState,
    WriteSubmissionState,
};
use pretty_assertions::assert_eq;
use support::chat_support::{
    ChatSetup, OpenChatEnv, all_entries, chat_setup, open_chat, text_of, tools_named, wait_for,
};
use support::harness_support::{add_hooks, add_section, add_task, add_tool};
use support::session_support::{ControlledStorage, context};
use support::task_support::{Deferred, aborted, deferred};
use tempfile::TempDir;

async fn sqlite_path() -> (TempDir, String) {
    let dir = TempDir::new().expect("create tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

/// Text of about `tokens` estimated tokens, starting with `label`.
fn tok_text(label: &str, tokens: usize) -> String {
    let pad = (tokens * 4).saturating_sub(label.len() + 1);
    format!("{label} {}", "x".repeat(pad))
}

#[derive(Clone)]
struct CapturedRequest {
    messages: Vec<Message>,
    options: Option<SimpleStreamOptions>,
    model: String,
}

type StepFn = Arc<dyn Fn(CapturedRequest) -> pi_js::BoxFuture<pi_js::Result<AssistantMessage>> + Send + Sync>;

#[derive(Clone)]
enum ScriptStep {
    Message(AssistantMessage),
    Factory(StepFn),
}

/// Scripted faux model that answers agent requests and summarization requests from separate queues.
#[derive(Clone)]
struct Script {
    agent: Arc<Mutex<VecDeque<ScriptStep>>>,
    summaries: Arc<Mutex<VecDeque<ScriptStep>>>,
    agent_requests: Arc<Mutex<Vec<CapturedRequest>>>,
    summary_requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl Script {
    fn push_agent(&self, step: ScriptStep) {
        self.agent.lock().expect("lock agent").push_back(step);
    }

    fn push_summary(&self, step: ScriptStep) {
        self.summaries.lock().expect("lock summaries").push_back(step);
    }

    fn agent_reqs(&self) -> Vec<CapturedRequest> {
        self.agent_requests.lock().expect("lock agent requests").clone()
    }

    fn summary_reqs(&self) -> Vec<CapturedRequest> {
        self.summary_requests.lock().expect("lock summary requests").clone()
    }
}

fn is_summary_request(messages: &[Message]) -> bool {
    let Some(Message::System(sys)) = messages.first() else {
        return false;
    };
    matches!(
        &sys.content,
        pi_ai::types::SystemMessageContent::Text(t)
            if t.starts_with("You are a context summarization assistant")
    )
}

fn attach_script(setup: &ChatSetup) -> Script {
    let result = Script {
        agent: Arc::new(Mutex::new(VecDeque::new())),
        summaries: Arc::new(Mutex::new(VecDeque::new())),
        agent_requests: Arc::new(Mutex::new(Vec::new())),
        summary_requests: Arc::new(Mutex::new(Vec::new())),
    };

    let steps: Vec<FauxResponseStep> = (0..500)
        .map(|_| {
            let script_ref = result.clone();
            let factory: FauxResponseFactory = Arc::new(move |transcript, options, _state, model| {
                let script = script_ref.clone();
                Box::pin(async move {
                    let req = CapturedRequest {
                        messages: transcript.messages.clone(),
                        options: options.clone(),
                        model: model.id.clone(),
                    };
                    let summary = is_summary_request(&req.messages);
                    if summary {
                        script
                            .summary_requests
                            .lock()
                            .expect("lock summary reqs")
                            .push(req.clone());
                    } else {
                        script.agent_requests.lock().expect("lock agent reqs").push(req.clone());
                    }
                    let next = if summary {
                        script.summaries.lock().expect("lock summaries").pop_front()
                    } else {
                        script.agent.lock().expect("lock agent").pop_front()
                    };
                    let Some(step) = next else {
                        return Err(pi_js::Error::msg(format!(
                            "No scripted {} response",
                            if summary { "summary" } else { "agent" }
                        )));
                    };
                    match step {
                        ScriptStep::Message(msg) => Ok(msg),
                        ScriptStep::Factory(f) => f(req).await,
                    }
                })
            });
            FauxResponseStep::Factory(factory)
        })
        .collect();
    setup.faux.set_responses(steps);
    result
}

/// A step that waits for `gate`, or rejects when the request is cancelled.
fn gated(gate: Deferred<()>, message: AssistantMessage, reached: Option<Deferred<()>>) -> ScriptStep {
    ScriptStep::Factory(Arc::new(move |req| {
        let gate = gate.clone();
        let message = message.clone();
        let reached = reached.clone();
        Box::pin(async move {
            if let Some(r) = reached {
                r.resolve(());
            }
            let signal = req.options.as_ref().and_then(|o| o.base.signal.as_ref()).cloned();
            tokio::select! {
                _ = gate.wait() => Ok(message),
                res = async {
                    match signal {
                        Some(sig) => aborted::<()>(&sig).await,
                        None => std::future::pending().await,
                    }
                } => Err(res.expect_err("aborted returns Err")),
            }
        })
    }))
}

fn answer(content: &str) -> AssistantMessage {
    faux_assistant_message(FauxAssistantMessageContent::Text(content.to_owned()), None)
}

fn failure(error_message: &str) -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Text(String::new()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(error_message.to_owned()),
            ..Default::default()
        }),
    )
}

fn summary_msg(content: &str) -> AssistantMessage {
    answer(content)
}

const OVERFLOW: &str = "prompt is too long: 250000 tokens > 200000 maximum";

fn manual_policy() -> CompactionPolicy {
    CompactionPolicy {
        enabled: false,
        reserve_tokens: 1000,
        keep_recent_tokens: 150,
        background_tokens: 0,
    }
}

fn partial_from(p: CompactionPolicy) -> PartialCompactionPolicy {
    PartialCompactionPolicy {
        enabled: Some(p.enabled),
        reserve_tokens: Some(p.reserve_tokens),
        keep_recent_tokens: Some(p.keep_recent_tokens),
        background_tokens: Some(p.background_tokens),
    }
}

struct Chat {
    harness: Harness,
    root: Conversation,
    setup: ChatSetup,
    faux: Script,
}

async fn open_chat_env(
    policy: Option<CompactionPolicy>,
    context_window: Option<i64>,
    storage: Option<Arc<dyn Storage>>,
    custom_setup: Option<ChatSetup>,
) -> Chat {
    let setup = custom_setup.unwrap_or_else(|| {
        chat_setup(Some(RegisterFauxProviderOptions {
            models: Some(vec![FauxModelDefinition {
                id: "faux-1".to_owned(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: Some(context_window.unwrap_or(100_000)),
                max_tokens: Some(900),
            }]),
            ..Default::default()
        }))
    });
    let faux = attach_script(&setup);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are helpful.".to_owned())) })),
        Some(pi_durable::harness::define::SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("add preamble");
    let store = storage.unwrap_or_else(|| Arc::new(MemoryStorage::new()));
    let opened = open_chat(store, &setup, OpenChatEnv::None).await.expect("open chat");
    let pol = policy.unwrap_or_else(manual_policy);
    setup.settings.update(|s| {
        s.compaction = Some(partial_from(pol));
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    opened.harness.resume().expect("resume");
    Chat {
        harness: opened.harness,
        root: opened.root,
        setup,
        faux,
    }
}

async fn turn(chat: &Chat, user_prompt: &str, reply: &str) {
    chat.faux.push_agent(ScriptStep::Message(answer(reply)));
    let submission = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(user_prompt.to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = submission.wait(context()).await.expect("wait");
    assert_eq!(settled.record.status(), pi_durable::types::SubmissionStatus::Done);
}

async fn history(chat: &Chat) {
    turn(chat, &tok_text("u1", 100), &tok_text("a1", 100)).await;
    turn(chat, &tok_text("u2", 100), &tok_text("a2", 100)).await;
    turn(chat, &tok_text("u3", 100), &tok_text("a3", 100)).await;
}

async fn task_result(chat: &Chat, id: TaskId<CompactionResult>) -> TaskOutcome<CompactionResult> {
    let record = chat.harness.wait_for_task(id, context()).await.expect("wait_for_task");
    record.outcome
}

async fn entry_kinds(conversation: &Conversation) -> Vec<String> {
    all_entries(conversation, None)
        .await
        .expect("all_entries")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

async fn live_doc(chat: &Chat) -> LiveState {
    chat.harness
        .snapshot_conversation(&*LIVE_DOC, chat.root.id(), context())
        .await
        .expect("live snapshot")
        .unwrap_or_default()
}

fn user_text(message: Option<&Message>) -> String {
    message.and_then(text_of).unwrap_or("").to_owned()
}

fn compaction_sub_id(outcome: &TaskOutcome<CompactionResult>) -> SubmissionId {
    match outcome {
        TaskOutcome::Completed { result } => result.submission_id.expect("submission_id"),
        other => panic!("Expected Completed compaction outcome, got {other:?}"),
    }
}

async fn compaction_tasks(chat: &Chat) -> Vec<pi_durable::types::ErasedTaskRecord> {
    let inspection = chat.harness.inspect(context()).await.expect("inspect");
    inspection
        .tasks
        .into_iter()
        .map(|t| t.record)
        .filter(|r| r.kind == "pi.compaction")
        .collect()
}

// ─── Range selection ──────────────────────────────────────────────────────

fn make_entry(id_counter: &mut i64, kind: &str, model: Vec<Message>, head: Option<i64>) -> EntryRecord {
    let id = *id_counter;
    *id_counter += 1;
    EntryRecord {
        id: EntryId::new_unchecked(id),
        conversation_id: pi_durable::types::ConversationId::new_unchecked(1),
        kind: kind.to_owned(),
        model: Some(model),
        data: None,
        head: head.map(EntryId::new_unchecked),
        edits: None,
        by_task_id: None,
    }
}

fn user_msg(content: &str) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(content.to_owned()),
        timestamp: 0,
    })
}

fn assistant_msg(content: &str, calls: &[&str], stop_reason: StopReason) -> Message {
    let mut blocks = vec![FauxContentBlock::Text(faux_text(content))];
    for id in calls {
        blocks.push(FauxContentBlock::ToolCall(faux_tool_call(
            "read",
            IndexMap::new(),
            Some(FauxToolCallOptions {
                id: Some((*id).to_owned()),
            }),
        )));
    }
    let mut msg = faux_assistant_message(FauxAssistantMessageContent::Blocks(blocks), None);
    msg.stop_reason = stop_reason;
    Message::Assistant(msg)
}

fn tool_result_msg(call_id: &str, content: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: call_id.to_owned(),
        tool_name: "read".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new(content))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
    })
}

fn build_view(entries: Vec<EntryRecord>, head: Option<EntryRecord>) -> ContextView {
    let mut all = Vec::new();
    if let Some(h) = head.clone() {
        all.push(h);
    }
    all.extend(entries);
    let contributions: Vec<Vec<Message>> = all
        .iter()
        .map(|record| {
            record
                .model
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|msg| match msg {
                    Message::Assistant(a) => !matches!(
                        a.stop_reason,
                        StopReason::Error | StopReason::Aborted | StopReason::Deferred
                    ),
                    _ => true,
                })
                .collect()
        })
        .collect();
    let flat: Vec<Message> = contributions.iter().flatten().cloned().collect();
    let messages = order_tool_results(&flat);
    ContextView {
        head,
        entries: all,
        contributions,
        messages,
    }
}

#[tokio::test]
async fn keeps_about_keep_recent_tokens_and_cuts_at_the_first_candidate_at_or_after_the_budget() {
    let mut id = 1;
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("1", 10))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg("2", &["c1"], StopReason::Stop)],
            None,
        ),
        make_entry(
            &mut id,
            "pi.tool-result",
            vec![tool_result_msg("c1", &tok_text("3", 3000))],
            None,
        ),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("4", 10), &[], StopReason::Stop)],
            None,
        ),
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("5", 10))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("6", 10), &[], StopReason::Stop)],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(entries, None), 2000), Some(3));
}

#[tokio::test]
async fn cuts_at_a_user_entry() {
    let mut id = 1;
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u1", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a1", 100), &[], StopReason::Stop)],
            None,
        ),
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u2", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a2", 100), &[], StopReason::Stop)],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(entries, None), 150), Some(2));
}

#[tokio::test]
async fn cuts_at_an_assistant_in_the_middle_of_one_long_run_and_never_at_a_tool_result() {
    let mut id = 1;
    let mut entries = vec![make_entry(&mut id, "pi.user", vec![user_msg("do it")], None)];
    for index in 0..5 {
        let call = format!("c{index}");
        entries.push(make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(
                &format!("step {index}"),
                &[call.as_str()],
                StopReason::Stop,
            )],
            None,
        ));
        entries.push(make_entry(
            &mut id,
            "pi.tool-result",
            vec![tool_result_msg(&call, &tok_text(&format!("r{index}"), 100))],
            None,
        ));
    }
    let view = build_view(entries.clone(), None);
    let cut = select_cut(&view, 150).expect("cut");
    assert_eq!(entries[cut].kind, "pi.assistant");
    assert_eq!(cut, 9);
}

#[tokio::test]
async fn keeps_a_huge_last_tool_result_together_with_its_assistant() {
    let mut id = 1;
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg("u")], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg("a", &["c"], StopReason::Stop)],
            None,
        ),
        make_entry(
            &mut id,
            "pi.tool-result",
            vec![tool_result_msg("c", &tok_text("big", 5000))],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(entries, None), 100), Some(1));
}

#[tokio::test]
async fn never_cuts_at_a_system_entry_or_an_excluded_error_or_aborted_answer() {
    let mut id = 1;
    let mut sections = IndexMap::new();
    sections.insert("s".to_owned(), Some(tok_text("s", 100)));
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u1", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a1", 100), &[], StopReason::Stop)],
            None,
        ),
        make_entry(
            &mut id,
            "pi.system",
            vec![Message::System(SystemMessage {
                content: SystemMessageContent::Text(String::new()),
                sections: Some(sections),
                tools_added: None,
                tools_removed: None,
                timestamp: 0,
            })],
            None,
        ),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("err", 100), &[], StopReason::Error)],
            None,
        ),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("stopped", 100), &[], StopReason::Aborted)],
            None,
        ),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a2", 100), &[], StopReason::Stop)],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(entries, None), 150), Some(5));
}

#[tokio::test]
async fn follows_edited_contributions_an_omitted_entry_adds_nothing_and_is_no_candidate() {
    let mut id = 1;
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u1", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a1", 100), &[], StopReason::Stop)],
            None,
        ),
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u2", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a2", 100), &[], StopReason::Stop)],
            None,
        ),
    ];
    let plain = build_view(entries, None);
    assert_eq!(select_cut(&plain, 150), Some(2));
    let mut contributions = plain.contributions.clone();
    contributions[2] = Vec::new();
    let flat: Vec<Message> = contributions.iter().flatten().cloned().collect();
    let omitted = ContextView {
        head: plain.head.clone(),
        entries: plain.entries.clone(),
        messages: order_tool_results(&flat),
        contributions,
    };
    assert_eq!(select_cut(&omitted, 150), Some(1));
}

#[tokio::test]
async fn does_not_cut_at_a_user_entry_that_a_result_of_the_preceding_call_still_follows() {
    let mut id = 1;
    let entries = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u1", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg("a", &["c"], StopReason::Stop)],
            None,
        ),
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("steer", 100))], None),
        make_entry(
            &mut id,
            "pi.tool-result",
            vec![tool_result_msg("c", &tok_text("r", 100))],
            None,
        ),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a2", 100), &[], StopReason::Stop)],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(entries, None), 250), Some(4));
}

#[tokio::test]
async fn finds_nothing_when_the_budget_is_never_reached_or_only_the_marker_precedes_the_cut() {
    let mut id = 1;
    let small = vec![
        make_entry(&mut id, "pi.user", vec![user_msg("hi")], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg("hello", &[], StopReason::Stop)],
            None,
        ),
    ];
    assert_eq!(select_cut(&build_view(small, None), 150), None);
    let marker = make_entry(&mut id, "pi.compaction", vec![user_msg("summary")], Some(0));
    let u = make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u", 200))], None);
    assert_eq!(select_cut(&build_view(vec![u], Some(marker)), 150), None);
}

#[tokio::test]
async fn summarizes_an_earlier_summary_marker_first() {
    let mut id = 1;
    let marker = make_entry(&mut id, "pi.compaction", vec![user_msg("EARLIER")], Some(0));
    let kept = vec![
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u1", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a1", 100), &[], StopReason::Stop)],
            None,
        ),
        make_entry(&mut id, "pi.user", vec![user_msg(&tok_text("u2", 100))], None),
        make_entry(
            &mut id,
            "pi.assistant",
            vec![assistant_msg(&tok_text("a2", 100), &[], StopReason::Stop)],
            None,
        ),
    ];
    let selected = build_view(kept, Some(marker));
    assert_eq!(select_cut(&selected, 150), Some(3));
    let prefix: Vec<Message> = selected.contributions[..3].iter().flatten().cloned().collect();
    assert!(serialize_conversation(&prefix).starts_with("[User]: EARLIER"));
}

// ─── Serialization ────────────────────────────────────────────────────────

#[tokio::test]
async fn writes_a_transcript_truncates_tool_results_and_omits_system_messages() {
    let mut args = IndexMap::new();
    args.insert("path".to_owned(), serde_json::json!("a.ts"));
    let call = faux_tool_call(
        "read",
        args,
        Some(FauxToolCallOptions {
            id: Some("c".to_owned()),
        }),
    );
    let mut sections = IndexMap::new();
    sections.insert("s".to_owned(), Some("hidden".to_owned()));
    let messages = vec![
        Message::System(SystemMessage {
            content: SystemMessageContent::Text(String::new()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        }),
        user_msg("hello"),
        Message::Assistant(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![
                FauxContentBlock::Thinking(faux_thinking("hmm")),
                FauxContentBlock::Text(faux_text("sure")),
                FauxContentBlock::ToolCall(call),
            ]),
            None,
        )),
        tool_result_msg("c", &"y".repeat(2500)),
    ];
    let serialized = serialize_conversation(&messages);
    assert!(!serialized.contains("hidden"));
    assert!(serialized.contains("[User]: hello"));
    assert!(serialized.contains("[Assistant thinking]: hmm"));
    assert!(serialized.contains("[Assistant]: sure"));
    assert!(serialized.contains("[Assistant tool calls]: read(path=\"a.ts\")"));
    assert!(serialized.contains(&format!(
        "[Tool result]: {}\n\n[... 500 more characters truncated]",
        "y".repeat(2000)
    )));
}

// ─── Manual compaction ────────────────────────────────────────────────────

#[tokio::test]
async fn places_the_summary_at_once_when_idle_and_keeps_raw_history() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let before = all_entries(&chat.root, None).await.expect("entries");
    let usage_before = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("usage doc")
        .models["faux/faux-1"]
        .clone();
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));

    let id = chat
        .root
        .compact(Some("focus on files".to_owned()), context())
        .await
        .expect("compact");
    let outcome = task_result(&chat, id).await;
    let submission_id = compaction_sub_id(&outcome);
    let placed = chat
        .harness
        .submission(submission_id, context())
        .await
        .expect("sub")
        .expect("sub present")
        .wait(context())
        .await
        .expect("wait");
    let marker_id_from_sub = match &placed.record.state {
        SubmissionState::Write(WriteSubmissionState::Done { entry }) => *entry,
        other => panic!("Expected Write Done, got {other:?}"),
    };

    let after = all_entries(&chat.root, None).await.expect("entries after");
    assert_eq!(&after[..before.len()], before.as_slice());
    let marker = after.last().expect("marker");
    let u3 = before
        .iter()
        .find(|r| user_text(r.model.as_ref().and_then(|m| m.first())).starts_with("u3"))
        .expect("u3");
    assert_eq!(marker.kind, "pi.compaction");
    assert_eq!(marker.head, Some(u3.id));
    assert_eq!(marker.data, Some(serde_json::json!({ "reason": "manual" })));
    assert_eq!(marker_id_from_sub, marker.id);
    assert_eq!(
        user_text(marker.model.as_ref().and_then(|m| m.first())),
        "The conversation history before this point was compacted into the following summary:\n\n<summary>\nSUMMARY\n</summary>"
    );

    let view = chat.root.context(context()).await.expect("context");
    let msg_texts: Vec<String> = view.messages.iter().map(|m| user_text(Some(m))).collect();
    assert_eq!(
        msg_texts,
        vec![
            user_text(marker.model.as_ref().and_then(|m| m.first())),
            tok_text("u3", 100),
            tok_text("a3", 100),
        ]
    );

    let sum_reqs = chat.faux.summary_reqs();
    let request = &sum_reqs[0];
    assert_eq!(request.messages.len(), 2);
    let prompt = user_text(Some(&request.messages[1]));
    assert!(prompt.starts_with("<conversation>\n[User]: u1 "));
    assert!(prompt.contains("[Assistant]: a2 "));
    assert!(!prompt.contains("u3 "));
    assert!(prompt.contains("## Goal"));
    assert!(prompt.ends_with("\n\nAdditional focus: focus on files"));
    let opts = request.options.as_ref().expect("options");
    assert_eq!(opts.base.cache_retention, Some(CacheRetention::None));
    assert_eq!(opts.base.max_tokens, Some(800));
    assert!(opts.deferred.is_none());

    let usage = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("usage doc")
        .models["faux/faux-1"]
        .clone();
    assert!(usage.input > usage_before.input);

    turn(&chat, "next", "done").await;
    let agent_reqs = chat.faux.agent_reqs();
    let agent = &agent_reqs.last().expect("last agent req").messages;
    let roles: Vec<&str> = agent
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::System(_) => "system",
            Message::ToolResult(_) => "toolResult",
        })
        .collect();
    assert_eq!(roles, vec!["user", "user", "assistant", "user", "system"]);
    match &agent[4] {
        Message::System(sys) => {
            assert_eq!(
                sys.sections
                    .as_ref()
                    .and_then(|s| s.get("preamble"))
                    .and_then(|v| v.as_deref()),
                Some("You are helpful.")
            );
        }
        other => panic!("Expected system message, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_working_while_busy_and_places_the_summary_at_the_next_final_boundary() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(gate.clone(), answer("late answer"), Some(reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let submission_id = compaction_sub_id(&outcome);
    let sub = chat
        .harness
        .submission(submission_id, context())
        .await
        .expect("get sub")
        .expect("sub exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait input").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert_eq!(
        sub.wait(context()).await.expect("wait sub").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let k = entry_kinds(&chat.root).await;
    assert_eq!(&k[k.len() - 3..], &["pi.user", "pi.assistant", "pi.compaction"]);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_a_queued_summary_at_post_tools_and_the_run_continues_in_the_compacted_context() {
    let chat = open_chat_env(None, None, None, None).await;
    add_tool(
        &chat.setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "wait".to_owned(),
            description: "wait".to_owned(),
            parameters: Type::object([] as [(&str, pi_ai::TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| {
                Box::pin(async {
                    Ok(ToolExecutionResult {
                        content: Some(vec![pi_ai::types::UserContentBlock::Text(TextContent::new("waited"))]),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add wait tool");
    chat.root
        .configure(
            AgentChange {
                tools: FieldChange::Set(AgentToolsChange::replace(&tools_named(&chat.setup, &["wait"]))),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure");
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux.push_agent(gated(
        gate.clone(),
        faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ),
        Some(reached.clone()),
    ));
    chat.faux.push_agent(ScriptStep::Message(answer("after tools")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("use a tool".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait input").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let agent_reqs = chat.faux.agent_reqs();
    let continuation = &agent_reqs.last().expect("continuation").messages;
    assert!(user_text(continuation.first()).contains("<summary>\nSUMMARY\n</summary>"));
    let k = entry_kinds(&chat.root).await;
    assert_eq!(
        &k[k.len() - 4..],
        &["pi.tool-result", "pi.compaction", "pi.system", "pi.assistant"]
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_follow_ups_left_by_a_failed_run_after_placing_the_summary() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(gate.clone(), failure("bad request"), Some(reached.clone())));
    let failed = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("fails".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let follow_up = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("follow-up".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit followup");
    gate.resolve(());
    assert_eq!(
        failed.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Unanswered
    );
    assert_eq!(
        follow_up.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );

    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    chat.faux.push_agent(ScriptStep::Message(answer("followed")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert_eq!(
        follow_up.wait(context()).await.expect("wait followup").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let reqs = chat.faux.agent_reqs();
    let request = &reqs.last().expect("req").messages;
    assert!(user_text(request.first()).contains("SUMMARY"));
    assert!(request.iter().any(|m| user_text(Some(m)) == "follow-up"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn settles_stale_when_a_reset_lands_while_it_summarizes() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    chat.root.reset(None, context()).await.expect("reset");
    gate.resolve(());
    let outcome = task_result(&chat, id).await;
    let submission_id = compaction_sub_id(&outcome);
    let status = chat
        .harness
        .submission(submission_id, context())
        .await
        .expect("sub")
        .expect("exists")
        .status(context())
        .await
        .expect("status");
    match status.state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "stale");
        }
        other => panic!("Expected stale Unanswered write, got {other:?}"),
    }
    assert_eq!(
        entry_kinds(&chat.root).await.last().map(String::as_str),
        Some("pi.reset")
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_make_the_conversation_busy_a_submission_during_summarization_starts_its_run_at_once() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let summary_gate = deferred::<()>();
    let summary_reached = deferred::<()>();
    chat.faux.push_summary(gated(
        summary_gate.clone(),
        summary_msg("SUMMARY"),
        Some(summary_reached.clone()),
    ));
    let id = chat.root.compact(None, context()).await.expect("compact");
    summary_reached.wait().await.expect("summary reached");
    let answer_gate = deferred::<()>();
    let answer_reached = deferred::<()>();
    chat.faux
        .push_agent(gated(answer_gate.clone(), answer("a4"), Some(answer_reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("u4".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit u4");
    assert_eq!(
        input.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Placed
    );
    answer_reached.wait().await.expect("answer reached");
    assert_eq!(
        user_text(chat.faux.agent_reqs().last().expect("req").messages.first()),
        tok_text("u1", 100)
    );
    summary_gate.resolve(());
    let outcome = task_result(&chat, id).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    answer_gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait input").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert_eq!(
        sub.wait(context()).await.expect("wait sub").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let k = entry_kinds(&chat.root).await;
    assert_eq!(&k[k.len() - 3..], &["pi.user", "pi.assistant", "pi.compaction"]);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn counts_the_spend_of_a_summary_that_ends_stale_and_writes_no_entry_for_it() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let input_tokens = || async {
        chat.harness
            .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
            .await
            .expect("usage")
            .expect("usage doc")
            .models["faux/faux-1"]
            .input
    };
    let before = input_tokens().await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    chat.root.reset(None, context()).await.expect("reset");
    gate.resolve(());
    let outcome = task_result(&chat, id).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    match sub.status(context()).await.expect("status").state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "stale"),
        other => panic!("Expected stale, got {other:?}"),
    }
    assert!(input_tokens().await > before);
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_the_compaction_that_cuts_furthest_win_whatever_finishes_first() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let first = deferred::<()>();
    let first_reached = deferred::<()>();
    chat.faux
        .push_summary(gated(first.clone(), summary_msg("FIRST"), Some(first_reached.clone())));
    let early = chat.root.compact(None, context()).await.expect("early");
    first_reached.wait().await.expect("first reached");
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SECOND")));
    let later = task_result(&chat, chat.root.compact(None, context()).await.expect("later")).await;
    assert!(matches!(later, TaskOutcome::Completed { .. }));
    first.resolve(());
    let outcome = task_result(&chat, early).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    match sub.status(context()).await.expect("status").state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "stale"),
        other => panic!("Expected stale, got {other:?}"),
    }
    let ctx = chat.root.context(context()).await.expect("ctx");
    assert!(user_text(ctx.messages.first()).contains("SECOND"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_an_older_selected_summary_that_cuts_later_than_the_newer_one() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("A"), Some(reached.clone())));
    let a = chat.root.compact(None, context()).await.expect("compact a");
    reached.wait().await.expect("reached");
    chat.setup.settings.update(|s| {
        let mut pol = manual_policy();
        pol.keep_recent_tokens = 350;
        s.compaction = Some(partial_from(pol));
    });
    chat.faux.push_summary(ScriptStep::Message(summary_msg("B")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact b")).await;
    assert!(user_text(chat.root.context(context()).await.expect("ctx").messages.first()).contains("B"));
    gate.resolve(());
    let outcome = task_result(&chat, a).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let messages = chat.root.context(context()).await.expect("ctx").messages;
    assert!(user_text(messages.first()).contains("<summary>\nA\n</summary>"));
    let rest: Vec<String> = messages[1..].iter().map(|m| user_text(Some(m))).collect();
    assert_eq!(rest, vec![tok_text("u3", 100), tok_text("a3", 100)]);
    chat.harness.close(context()).await.expect("close");
}

async fn two_queued_summaries_case(keeps: [u64; 2], stale: bool) {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(gate.clone(), answer("done"), Some(reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let mut submissions = Vec::new();
    for (index, keep) in keeps.into_iter().enumerate() {
        chat.setup.settings.update(|s| {
            let mut pol = manual_policy();
            pol.keep_recent_tokens = keep;
            s.compaction = Some(partial_from(pol));
        });
        chat.faux
            .push_summary(ScriptStep::Message(summary_msg(&format!("S{index}"))));
        let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
        submissions.push(compaction_sub_id(&outcome));
    }
    gate.resolve(());
    input.wait(context()).await.expect("wait");
    let mut statuses = Vec::new();
    for id in submissions {
        let settled = chat
            .harness
            .submission(id, context())
            .await
            .expect("sub")
            .expect("exists")
            .wait(context())
            .await
            .expect("wait");
        statuses.push(settled.record.status());
    }
    let expected = [
        pi_durable::types::SubmissionStatus::Done,
        if stale {
            pi_durable::types::SubmissionStatus::Unanswered
        } else {
            pi_durable::types::SubmissionStatus::Done
        },
    ];
    assert_eq!(statuses, expected);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_two_queued_summaries_in_one_boundary_when_the_second_cuts_before_the_first() {
    two_queued_summaries_case([150, 350], true).await;
}

#[tokio::test]
async fn places_two_queued_summaries_in_one_boundary_when_the_second_cuts_at_the_first() {
    two_queued_summaries_case([150, 150], false).await;
}

#[tokio::test]
async fn places_two_queued_summaries_in_one_boundary_when_the_second_cuts_after_the_first() {
    two_queued_summaries_case([350, 150], false).await;
}

#[tokio::test]
async fn is_aborted_by_conversation_abort_an_already_queued_summary_survives_it() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    assert_eq!(live_doc(&chat).await.compactions.as_ref().map(|v| v.len()), Some(1));
    chat.root.abort(context(), None).await.expect("abort");
    assert!(matches!(task_result(&chat, id).await, TaskOutcome::Aborted { .. }));
    assert!(live_doc(&chat).await.compactions.is_none());
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));

    let gate = deferred::<()>();
    let busy_reached = deferred::<()>();
    chat.faux
        .push_agent(gated(gate, answer("never"), Some(busy_reached.clone())));
    chat.root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    busy_reached.wait().await.expect("busy");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let queued = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    chat.root.abort(context(), None).await.expect("abort");
    let sub = chat
        .harness
        .submission(compaction_sub_id(&queued), context())
        .await
        .expect("sub")
        .expect("exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    assert_eq!(
        sub.abort(context()).await.expect("abort sub"),
        pi_durable::harness::types::AbortSubmissionResult::Aborted
    );
    turn(&chat, "next", "ok").await;
    match sub.status(context()).await.expect("status").state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("Expected aborted write, got {other:?}"),
    }
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn is_ordinary_work_idle_waits_include_it() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    let idle = Arc::new(AtomicBool::new(false));
    let idle_flag = Arc::clone(&idle);
    let root_clone = chat.root.clone();
    let wait = tokio::spawn(async move {
        root_clone.wait_for_idle(context()).await.expect("wait idle");
        idle_flag.store(true, Ordering::Release);
    });
    let _ = pi_js::time::sleep(20, None).await;
    assert!(!idle.load(Ordering::Acquire));
    gate.resolve(());
    wait.await.expect("join");
    let task = chat
        .harness
        .get_task(id, context())
        .await
        .expect("get_task")
        .expect("exists");
    assert_eq!(task.state.status(), pi_durable::types::TaskStatus::Terminal);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn enables_scheduling_right_after_open() {
    let setup = chat_setup(None);
    let faux = attach_script(&setup);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    setup.settings.update(|s| {
        let mut pol = manual_policy();
        pol.keep_recent_tokens = 10;
        s.compaction = Some(partial_from(pol));
    });
    let id = opened.root.compact(None, context()).await.expect("compact");
    wait_for(
        || {
            let h = opened.harness.clone();
            async move {
                h.get_task(id, context())
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|t| t.state.status() == pi_durable::types::TaskStatus::Terminal)
            }
        },
        5_000,
    )
    .await;
    let task = opened
        .harness
        .get_task(id, context())
        .await
        .expect("get_task")
        .expect("exists");
    assert_eq!(
        task.state.outcome(),
        Some(&TaskOutcome::Completed {
            result: CompactionResult {
                entry_id: None,
                submission_id: None,
            }
        })
    );
    assert_eq!(faux.summary_reqs().len(), 0);
    opened.harness.close(context()).await.expect("close");
}

// ─── Compaction outcomes ──────────────────────────────────────────────────

#[tokio::test]
async fn completes_without_a_summary_when_there_is_nothing_to_compact() {
    let chat = open_chat_env(None, None, None, None).await;
    turn(&chat, "hi", "hello").await;
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert_eq!(
        outcome,
        TaskOutcome::Completed {
            result: CompactionResult {
                entry_id: None,
                submission_id: None,
            }
        }
    );
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn asks_before_compact_the_first_decision_wins_a_throw_is_reported_and_skipped() {
    let chat = open_chat_env(None, None, None, None).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(|_, _, _| {
                Box::pin(async { Err(pi_js::Error::msg("hook broke")) })
            })),
        },
        None,
    )
    .expect("hook 1");
    let seen_cb = Arc::clone(&seen);
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |compaction, _, _| {
                let seen_cb = Arc::clone(&seen_cb);
                Box::pin(async move {
                    seen_cb.lock().expect("lock seen").push(compaction);
                    Ok(Some(BeforeCompactDecision::Summary("FROM HOOK".to_owned())))
                })
            })),
        },
        None,
    )
    .expect("hook 2");
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(BeforeCompactDecision::Decline)) })
            })),
        },
        None,
    )
    .expect("hook 3");

    history(&chat).await;
    let outcome = task_result(
        &chat,
        chat.root
            .compact(Some("why".to_owned()), context())
            .await
            .expect("compact"),
    )
    .await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    assert!(
        user_text(chat.root.context(context()).await.expect("ctx").messages.first())
            .contains("<summary>\nFROM HOOK\n</summary>")
    );
    let reports: Vec<String> = chat
        .setup
        .reports
        .lock()
        .expect("lock reports")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].contains("hook broke"));
    let seen_list = seen.lock().expect("lock seen");
    let compaction = &seen_list[0];
    assert_eq!(compaction.reason, CompactionReason::Manual);
    assert_eq!(compaction.instructions.as_deref(), Some("why"));
    let entry_kind_list: Vec<&str> = compaction.entries.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(
        entry_kind_list,
        vec!["pi.user", "pi.system", "pi.assistant", "pi.user", "pi.assistant"]
    );
    let msg_texts: Vec<String> = compaction
        .messages
        .iter()
        .filter(|m| !matches!(m, Message::System(_)))
        .map(|m| user_text(Some(m)))
        .collect();
    assert_eq!(
        msg_texts,
        vec![
            tok_text("u1", 100),
            tok_text("a1", 100),
            tok_text("u2", 100),
            tok_text("a2", 100)
        ]
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn completes_without_a_summary_when_a_hook_declines() {
    let chat = open_chat_env(None, None, None, None).await;
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(BeforeCompactDecision::Decline)) })
            })),
        },
        None,
    )
    .expect("hook");
    history(&chat).await;
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert_eq!(
        outcome,
        TaskOutcome::Completed {
            result: CompactionResult {
                entry_id: None,
                submission_id: None,
            }
        }
    );
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_no_model_without_a_configured_model() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.root
        .configure(
            AgentChange {
                model: FieldChange::Clear,
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("clear model");
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    match outcome {
        TaskOutcome::Failed { error, .. } => {
            assert_eq!(
                error
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str()),
                Some("no_model")
            );
        }
        other => panic!("Expected Failed no_model, got {other:?}"),
    }
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

async fn compaction_input(chat: &Chat) -> i64 {
    chat.harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("usage doc")
        .models["faux/faux-1"]
        .input
}

#[tokio::test]
async fn retries_a_retryable_error_with_the_pinned_request_and_counts_every_attempt_once() {
    let single = open_chat_env(None, None, None, None).await;
    history(&single).await;
    single
        .root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Set(pi_ai::types::ModelThinkingLevel::High),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure");
    single.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let before_single = compaction_input(&single).await;
    task_result(&single, single.root.compact(None, context()).await.expect("compact")).await;
    let once = compaction_input(&single).await - before_single;
    assert!(once > 0);
    single.harness.close(context()).await.expect("close");

    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.root
        .configure(
            AgentChange {
                thinking_level: FieldChange::Set(pi_ai::types::ModelThinkingLevel::High),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure");
    chat.setup.settings.update(|s| {
        s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
            timeout_ms: Some(1234),
            deferred: Some(StreamDeferredOption::Bool(true)),
            ..Default::default()
        });
    });
    let root_cb = chat.root.clone();
    let settings_cb = Arc::clone(&chat.setup.settings);
    chat.faux.push_summary(ScriptStep::Factory(Arc::new(move |_| {
        let root_cb = root_cb.clone();
        let settings_cb = Arc::clone(&settings_cb);
        Box::pin(async move {
            root_cb
                .configure(
                    AgentChange {
                        thinking_level: FieldChange::Set(pi_ai::types::ModelThinkingLevel::Low),
                        ..Default::default()
                    },
                    context(),
                )
                .await?;
            settings_cb.update(|s| {
                s.stream = Some(pi_durable::harness::types::ConversationStreamOptions {
                    timeout_ms: Some(1),
                    ..Default::default()
                });
            });
            Ok(failure("overloaded"))
        })
    })));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let before_twice = compaction_input(&chat).await;
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    let twice = compaction_input(&chat).await - before_twice;
    let sum_reqs = chat.faux.summary_reqs();
    assert_eq!(sum_reqs.len(), 2);
    assert_eq!(twice, 2 * once);
    for request in &sum_reqs {
        let opts = request.options.as_ref().expect("opts");
        assert_eq!(opts.reasoning, Some(pi_ai::types::ThinkingLevel::High));
        assert_eq!(opts.base.timeout_ms, Some(1234));
        assert_eq!(opts.base.cache_retention, Some(CacheRetention::None));
        assert!(opts.deferred.is_none());
    }
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn adds_no_usage_when_a_hook_declines_or_supplies_the_summary() {
    for decision in [
        BeforeCompactDecision::Decline,
        BeforeCompactDecision::Summary("HOOK".to_owned()),
    ] {
        let chat = open_chat_env(None, None, None, None).await;
        add_hooks(
            &chat.setup.registry,
            &COMPACTION_TASK,
            CompactionHooks {
                before_compact: Some(Arc::new(move |_, _, _| {
                    let decision = decision.clone();
                    Box::pin(async move { Ok(Some(decision)) })
                })),
            },
            None,
        )
        .expect("hook");
        history(&chat).await;
        let before = compaction_input(&chat).await;
        task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
        assert_eq!(compaction_input(&chat).await - before, 0);
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn caps_max_tokens_at_the_models_output_limit_and_sends_no_tools() {
    let chat = open_chat_env(None, None, None, None).await;
    add_tool(
        &chat.setup.registry,
        define_tool(pi_durable::harness::types::ToolRegistration {
            name: "read".to_owned(),
            description: "read".to_owned(),
            parameters: Type::object([] as [(&str, pi_ai::TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
        }),
        None,
    )
    .expect("add tool");
    chat.root
        .configure(
            AgentChange {
                tools: FieldChange::Set(AgentToolsChange::replace(&tools_named(&chat.setup, &["read"]))),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure");
    history(&chat).await;
    chat.setup.settings.update(|s| {
        let mut pol = manual_policy();
        pol.reserve_tokens = 2000;
        s.compaction = Some(partial_from(pol));
    });
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let sum_reqs = chat.faux.summary_reqs();
    let request = &sum_reqs[0];
    assert_eq!(request.options.as_ref().and_then(|o| o.base.max_tokens), Some(900));
    assert_eq!(request.messages.len(), 2);
    assert!(!request.messages.iter().any(|m| match m {
        Message::System(sys) => sys.tools_added.is_some(),
        _ => false,
    }));
    chat.harness.close(context()).await.expect("close");
}

async fn fail_model_error_case(response: AssistantMessage, expected_msg: &str, expected_reqs: usize) {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    for _ in 0..3 {
        chat.faux.push_summary(ScriptStep::Message(response.clone()));
    }
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    match outcome {
        TaskOutcome::Failed { error, .. } => {
            assert_eq!(error.message, expected_msg);
            assert_eq!(
                error
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str()),
                Some("model_error")
            );
        }
        other => panic!("Expected Failed model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.summary_reqs().len(), expected_reqs);
    assert!(live_doc(&chat).await.compactions.is_none());
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_model_error_on_retries_run_out() {
    fail_model_error_case(failure("overloaded"), "Summarization failed: overloaded", 3).await;
}

#[tokio::test]
async fn fails_with_model_error_on_a_non_retryable_error() {
    fail_model_error_case(failure("bad request"), "Summarization failed: bad request", 1).await;
}

#[tokio::test]
async fn fails_with_model_error_on_a_length_stop() {
    fail_model_error_case(
        faux_assistant_message(
            FauxAssistantMessageContent::Text("partial".to_owned()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        ),
        "Summarization hit the token limit; the summary is incomplete",
        1,
    )
    .await;
}

#[tokio::test]
async fn fails_with_model_error_on_a_tool_call() {
    fail_model_error_case(
        faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "read",
                IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Stop),
                ..Default::default()
            }),
        ),
        "Summarization attempted to call a tool",
        1,
    )
    .await;
}

#[tokio::test]
async fn fails_with_model_error_on_empty_text() {
    fail_model_error_case(answer("  "), "Summarization produced no text", 1).await;
}

// ─── Automatic compaction ─────────────────────────────────────────────────

fn background_policy() -> CompactionPolicy {
    CompactionPolicy {
        enabled: true,
        reserve_tokens: 500,
        keep_recent_tokens: 150,
        background_tokens: 1000,
    }
}

fn blocking_policy() -> CompactionPolicy {
    CompactionPolicy {
        enabled: true,
        reserve_tokens: 300,
        keep_recent_tokens: 150,
        background_tokens: 0,
    }
}

#[tokio::test]
async fn starts_above_the_background_threshold_without_blocking_the_run_idle_waits_and_esc_ignore_it() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(background_policy())));
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    reached.wait().await.expect("reached");
    let tasks = compaction_tasks(&chat).await;
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert!(task.background);
    assert_eq!(task.input.get("reason").and_then(|v| v.as_str()), Some("threshold"));
    assert!(task.owner.is_none());
    assert_eq!(
        live_doc(&chat).await.compactions,
        Some(vec![CompactionStatus {
            task_id: TaskId::new_unchecked(task.id.as_i64()),
            reason: CompactionReason::Threshold,
            blocking: false,
            attempt: 1,
            retry: None,
        }])
    );

    chat.root.wait_for_idle(context()).await.expect("idle");
    chat.root.abort(context(), None).await.expect("abort");
    let running = chat
        .harness
        .get_erased_task(task.id, context())
        .await
        .expect("get task")
        .expect("exists");
    assert_eq!(running.state.status(), pi_durable::types::TaskStatus::Running);
    gate.resolve(());
    let outcome = task_result(&chat, TaskId::new_unchecked(task.id.as_i64())).await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    assert_eq!(
        entry_kinds(&chat.root).await.last().map(String::as_str),
        Some("pi.compaction")
    );
    assert!(user_text(chat.root.context(context()).await.expect("ctx").messages.first()).contains("SUMMARY"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_start_when_disabled() {
    let mut pol = background_policy();
    pol.enabled = false;
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    assert!(compaction_tasks(&chat).await.is_empty());
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_start_when_background_tokens_is_0() {
    let mut pol = background_policy();
    pol.background_tokens = 0;
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    assert!(compaction_tasks(&chat).await.is_empty());
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_start_when_there_is_no_cut() {
    let mut pol = background_policy();
    pol.keep_recent_tokens = 100_000;
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    assert!(compaction_tasks(&chat).await.is_empty());
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_start_while_another_compaction_is_listed() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let manual = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(background_policy())));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    let ids: Vec<TaskId> = compaction_tasks(&chat).await.into_iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![manual.erased()]);
    chat.harness
        .abort_task(manual.erased(), context())
        .await
        .expect("abort");
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stops_through_abort_task_and_conversation_abort_with_background() {
    for use_task_abort in [true, false] {
        let chat = open_chat_env(None, Some(2000), None, None).await;
        history(&chat).await;
        chat.setup
            .settings
            .update(|s| s.compaction = Some(partial_from(background_policy())));
        let reached = deferred::<()>();
        chat.faux
            .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
        turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
        reached.wait().await.expect("reached");
        let task = compaction_tasks(&chat).await.into_iter().next().expect("task");
        if use_task_abort {
            chat.harness.abort_task(task.id, context()).await.expect("abort task");
        } else {
            chat.root
                .abort(
                    context(),
                    Some(pi_durable::harness::types::ConversationAbortOptions { background: true }),
                )
                .await
                .expect("abort bg");
        }
        assert!(matches!(
            task_result(&chat, TaskId::new_unchecked(task.id.as_i64())).await,
            TaskOutcome::Aborted { .. }
        ));
        assert!(live_doc(&chat).await.compactions.is_none());
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn waits_for_its_compaction_which_appends_the_summary_before_the_request() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    add_section(
        &chat.setup.registry,
        "extra",
        Arc::new(|_, _| Box::pin(async { Ok(Some("EXTRA".to_owned())) })),
        None,
        None,
    )
    .expect("extra");
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let child = compaction_tasks(&chat).await.into_iter().next().expect("child");
    let generation = live_doc(&chat).await.run.expect("run").task_id;
    assert_eq!(child.owner, Some(generation));
    assert!(!child.background);
    assert_eq!(child.input.get("reason").and_then(|v| v.as_str()), Some("threshold"));
    let gen_record = chat
        .harness
        .get_erased_task(generation, context())
        .await
        .expect("gen task")
        .expect("exists");
    match &gen_record.state {
        TaskState::Waiting { checkpoint, on, .. } => {
            assert_eq!(on, &vec![child.id]);
            assert_eq!(checkpoint.get("phase").and_then(|v| v.as_str()), Some("prepare"));
            assert_eq!(checkpoint.get("attempt").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(
                checkpoint.get("compacted").and_then(|v| v.as_i64()),
                Some(child.id.as_i64())
            );
        }
        other => panic!("Expected waiting generation, got {other:?}"),
    }
    assert_eq!(
        live_doc(&chat).await.compactions,
        Some(vec![CompactionStatus {
            task_id: TaskId::new_unchecked(child.id.as_i64()),
            reason: CompactionReason::Threshold,
            blocking: true,
            attempt: 1,
            retry: None,
        }])
    );
    assert_eq!(
        entry_kinds(&chat.root).await.last().map(String::as_str),
        Some("pi.user")
    );
    gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert!(matches!(
        task_result(&chat, TaskId::new_unchecked(child.id.as_i64())).await,
        TaskOutcome::Completed { .. }
    ));
    let k = entry_kinds(&chat.root).await;
    assert_eq!(&k[k.len() - 3..], &["pi.compaction", "pi.system", "pi.assistant"]);
    let agent_reqs = chat.faux.agent_reqs();
    let request = &agent_reqs.last().expect("req").messages;
    assert!(user_text(request.first()).contains("SUMMARY"));
    let systems: Vec<&pi_ai::types::SystemMessage> = request
        .iter()
        .filter_map(|m| match m {
            Message::System(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(systems.len(), 1);
    let secs = systems[0].sections.as_ref().expect("sections");
    assert_eq!(
        secs.get("preamble").and_then(|v| v.as_deref()),
        Some("You are helpful.")
    );
    assert_eq!(
        secs.get("extra").and_then(|v| v.as_deref()),
        Some("<extra>\nEXTRA\n</extra>")
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sends_the_request_once_without_a_second_compaction_when_the_kept_part_is_still_above_the_threshold() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    let mut pol = blocking_policy();
    pol.keep_recent_tokens = 700;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    turn(&chat, &tok_text("u4", 400), "a4").await;
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    assert_eq!(
        entry_kinds(&chat.root)
            .await
            .into_iter()
            .filter(|k| k == "pi.compaction")
            .count(),
        1
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sends_the_request_anyway_when_its_compaction_declines() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(BeforeCompactDecision::Decline)) })
            })),
        },
        None,
    )
    .expect("hook");
    turn(&chat, &tok_text("u4", 200), "a4").await;
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    assert_eq!(
        user_text(chat.faux.agent_reqs().last().expect("req").messages.first()),
        tok_text("u1", 100)
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sends_the_request_anyway_when_its_compaction_fails() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    chat.faux.push_summary(ScriptStep::Message(failure("bad request")));
    turn(&chat, &tok_text("u4", 200), "a4").await;
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    assert_eq!(
        user_text(chat.faux.agent_reqs().last().expect("req").messages.first()),
        tok_text("u1", 100)
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sends_the_request_anyway_when_its_compaction_is_aborted_directly() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let child = compaction_tasks(&chat).await.into_iter().next().expect("child");
    chat.harness.abort_task(child.id, context()).await.expect("abort");
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn is_aborted_with_its_generation_by_esc_before_the_generations_abort_handler() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_cb = Arc::clone(&events);
    let stream = chat.root.watch_events(context()).await.expect("watch");
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().expect("lock events").extend(batch);
                Ok(())
            })
        })
        .expect("start");
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let child = compaction_tasks(&chat).await.into_iter().next().expect("child");
    chat.root.abort(context(), None).await.expect("abort");
    let settled = input.wait(context()).await.expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("Expected aborted, got {other:?}"),
    }
    let child_after = chat
        .harness
        .get_erased_task(child.id, context())
        .await
        .expect("get task")
        .expect("exists");
    assert!(matches!(child_after.state.outcome(), Some(TaskOutcome::Aborted { .. })));
    wait_for(
        || {
            let events = Arc::clone(&events);
            async move {
                events
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|e| matches!(e, AgentEvent::RunEnd { .. }))
            }
        },
        5_000,
    )
    .await;
    let ev_guard = events.lock().expect("lock");
    let end_idx = ev_guard
        .iter()
        .position(|e| matches!(e, AgentEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    let run_end_idx = ev_guard
        .iter()
        .position(|e| matches!(e, AgentEvent::RunEnd { .. }))
        .expect("run_end");
    assert!(end_idx < run_end_idx);
    drop(ev_guard);
    stream.stop().await;
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn wins_over_a_background_compaction_still_in_flight_which_then_settles_stale() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(background_policy())));
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("BACKGROUND"), Some(reached.clone())));
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    reached.wait().await.expect("reached");
    let background = compaction_tasks(&chat).await.into_iter().next().expect("bg");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("BLOCKING")));
    turn(&chat, &tok_text("u5", 1000), "a5").await;
    assert!(user_text(chat.root.context(context()).await.expect("ctx").messages.first()).contains("BLOCKING"));
    gate.resolve(());
    let outcome = task_result(&chat, TaskId::new_unchecked(background.id.as_i64())).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    match sub.status(context()).await.expect("status").state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "stale"),
        other => panic!("Expected stale, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
}

// ─── Overflow compaction ──────────────────────────────────────────────────

fn enabled_manual() -> CompactionPolicy {
    let mut pol = manual_policy();
    pol.enabled = true;
    pol
}

#[tokio::test]
async fn compacts_and_retries_with_the_same_attempt_leaving_the_error_out_of_the_retry() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let attempt = Arc::new(Mutex::new(None));
    let attempt_cb = Arc::clone(&attempt);
    let harness_cb = chat.harness.clone();
    let root_id = chat.root.id();
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    chat.faux.push_agent(ScriptStep::Factory(Arc::new(move |_| {
        let harness_cb = harness_cb.clone();
        let attempt_cb = Arc::clone(&attempt_cb);
        Box::pin(async move {
            let live = harness_cb
                .snapshot_conversation(&*LIVE_DOC, root_id, context())
                .await?
                .unwrap_or_default();
            *attempt_cb.lock().expect("lock") = live.generation.map(|g| g.attempt);
            Ok(answer("fits"))
        })
    })));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert_eq!(*attempt.lock().expect("lock"), Some(1));
    let k = entry_kinds(&chat.root).await;
    assert_eq!(
        &k[k.len() - 5..],
        &["pi.user", "pi.assistant", "pi.compaction", "pi.system", "pi.assistant"]
    );
    let entries = all_entries(&chat.root, None).await.expect("entries");
    let compaction = entries
        .into_iter()
        .find(|e| e.kind == "pi.compaction")
        .expect("compaction");
    assert_eq!(compaction.data, Some(serde_json::json!({ "reason": "overflow" })));
    let agent_reqs = chat.faux.agent_reqs();
    let retry = &agent_reqs.last().expect("retry").messages;
    assert!(user_text(retry.first()).contains("SUMMARY"));
    assert!(!retry.iter().any(|m| match m {
        Message::Assistant(a) => a.stop_reason == StopReason::Error,
        _ => false,
    }));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_a_second_overflow_with_its_error_entry() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = input.wait(context()).await.expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected Unanswered model_error, got {other:?}"),
    }
    let k = entry_kinds(&chat.root).await;
    assert_eq!(k.iter().filter(|s| s.as_str() == "pi.compaction").count(), 1);
    assert_eq!(k.last().map(String::as_str), Some("pi.assistant"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_without_compacting_when_compaction_is_disabled_even_for_a_retryable_looking_overflow() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.faux
        .push_agent(ScriptStep::Message(failure(&format!("overloaded: {OVERFLOW}"))));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = input.wait(context()).await.expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "model_error");
        }
        other => panic!("Expected model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.agent_reqs().len(), 4);
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_after_a_blocking_threshold_compaction_in_the_same_generation() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = input.wait(context()).await.expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => {
            assert_eq!(reason, "model_error");
        }
        other => panic!("Expected model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_the_overflow_text_when_compaction_declines() {
    let chat = open_chat_env(None, None, None, None).await;
    turn(&chat, &tok_text("u1", 100), &tok_text("a1", 100)).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(|_, _, _| {
                Box::pin(async { Ok(Some(BeforeCompactDecision::Decline)) })
            })),
        },
        None,
    )
    .expect("hook");
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    match input.wait(context()).await.expect("wait").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_the_overflow_text_when_compaction_fails() {
    let chat = open_chat_env(None, None, None, None).await;
    turn(&chat, &tok_text("u1", 100), &tok_text("a1", 100)).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    chat.faux.push_summary(ScriptStep::Message(failure("bad request")));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    match input.wait(context()).await.expect("wait").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_with_the_overflow_text_when_compaction_cannot_cut() {
    let chat = open_chat_env(None, None, None, None).await;
    turn(&chat, &tok_text("u1", 100), &tok_text("a1", 100)).await;
    let mut pol = enabled_manual();
    pol.keep_recent_tokens = 100_000;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    match input.wait(context()).await.expect("wait").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected model_error, got {other:?}"),
    }
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

// ─── Compaction estimates and interactions ────────────────────────────────

#[tokio::test]
async fn ignores_usage_measured_before_a_summary_placed_mid_run_real_and_fixed_clock() {
    for fixed_clock in [false, true] {
        let setup = chat_setup(Some(RegisterFauxProviderOptions {
            models: Some(vec![FauxModelDefinition {
                id: "faux-1".to_owned(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: Some(2000),
                max_tokens: Some(900),
            }]),
            ..Default::default()
        }));
        if fixed_clock {
            setup.now.set_fixed(1_000);
        }
        let chat = open_chat_env(None, Some(2000), None, Some(setup)).await;
        let tool_gate = deferred::<()>();
        let tool_reached = deferred::<()>();
        let gate_cb = tool_gate.clone();
        let reached_cb = tool_reached.clone();
        add_tool(
            &chat.setup.registry,
            define_tool(pi_durable::harness::types::ToolRegistration {
                name: "slow".to_owned(),
                description: "slow".to_owned(),
                parameters: Type::object([] as [(&str, pi_ai::TSchema); 0]).to_value(),
                replay: None,
                execution_mode: None,
                prepare_arguments: None,
                output_limits: None,
                execute: Arc::new(move |_, _, _| {
                    let gate_cb = gate_cb.clone();
                    let reached_cb = reached_cb.clone();
                    Box::pin(async move {
                        reached_cb.resolve(());
                        gate_cb.wait().await?;
                        Ok(ToolExecutionResult {
                            content: Some(vec![pi_ai::types::UserContentBlock::Text(TextContent::new(tok_text(
                                "result", 200,
                            )))]),
                            ..Default::default()
                        })
                    })
                }),
            }),
            None,
        )
        .expect("add tool");
        chat.root
            .configure(
                AgentChange {
                    tools: FieldChange::Set(AgentToolsChange::replace(&tools_named(&chat.setup, &["slow"]))),
                    ..Default::default()
                },
                context(),
            )
            .await
            .expect("configure");
        turn(&chat, &tok_text("u1", 220), &tok_text("a1", 220)).await;
        turn(&chat, &tok_text("u2", 220), &tok_text("a2", 220)).await;
        turn(&chat, &tok_text("u3", 220), &tok_text("a3", 220)).await;
        let mut pol = background_policy();
        pol.background_tokens = 600;
        chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
        chat.faux.push_agent(ScriptStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "slow",
                IndexMap::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )));
        chat.faux.push_agent(ScriptStep::Message(answer("done")));
        let summary_gate = deferred::<()>();
        let summary_reached = deferred::<()>();
        chat.faux.push_summary(gated(
            summary_gate.clone(),
            summary_msg("SUMMARY"),
            Some(summary_reached.clone()),
        ));
        let input = chat
            .root
            .submit(
                SubmissionDraft::Input {
                    request_id: None,
                    content: UserInput::Text(tok_text("u4", 50)),
                    when_busy: None,
                },
                context(),
            )
            .await
            .expect("submit");
        tokio::try_join!(tool_reached.wait(), summary_reached.wait()).expect("reached");
        let background = compaction_tasks(&chat).await.into_iter().next().expect("bg");
        summary_gate.resolve(());
        let queued = task_result(&chat, TaskId::new_unchecked(background.id.as_i64())).await;
        let sub = chat
            .harness
            .submission(compaction_sub_id(&queued), context())
            .await
            .expect("sub")
            .expect("exists");
        assert_eq!(
            sub.status(context()).await.expect("status").status(),
            pi_durable::types::SubmissionStatus::Queued
        );
        tool_gate.resolve(());
        assert_eq!(
            input.wait(context()).await.expect("wait").record.status(),
            pi_durable::types::SubmissionStatus::Done
        );
        assert_eq!(chat.faux.summary_reqs().len(), 1);
        assert!(compaction_tasks(&chat).await.is_empty());
        assert!(user_text(chat.faux.agent_reqs().last().expect("req").messages.first()).contains("SUMMARY"));
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn rebaselines_the_system_prompt_over_kept_system_deltas() {
    let chat = open_chat_env(None, None, None, None).await;
    let mood = Arc::new(Mutex::new("cheerful".to_owned()));
    let mood_cb = Arc::clone(&mood);
    add_section(
        &chat.setup.registry,
        "mood",
        Arc::new(move |_, _| {
            let text = mood_cb.lock().expect("lock mood").clone();
            Box::pin(async move { Ok(Some(text)) })
        }),
        Some(pi_durable::harness::define::SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("mood");
    turn(&chat, &tok_text("u1", 100), &tok_text("a1", 100)).await;
    turn(&chat, &tok_text("u2", 100), &tok_text("a2", 100)).await;
    *mood.lock().expect("lock mood") = "terse".to_owned();
    turn(&chat, &tok_text("u3", 100), &tok_text("a3", 100)).await;
    let mut pol = manual_policy();
    pol.keep_recent_tokens = 250;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert!(
        chat.root
            .context(context())
            .await
            .expect("ctx")
            .entries
            .iter()
            .any(|r| r.kind == "pi.system")
    );
    turn(&chat, "next", "ok").await;
    let agent_reqs = chat.faux.agent_reqs();
    let systems: Vec<&pi_ai::types::SystemMessage> = agent_reqs
        .last()
        .expect("req")
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::System(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(systems.len(), 1);
    let secs = systems[0].sections.as_ref().expect("sections");
    assert_eq!(
        secs.get("preamble").and_then(|v| v.as_deref()),
        Some("You are helpful.")
    );
    assert_eq!(secs.get("mood").and_then(|v| v.as_deref()), Some("terse"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn summarizes_a_replaced_entrys_replacement_and_shows_it_to_the_hook() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let u1 = all_entries(&chat.root, None)
        .await
        .expect("entries")
        .into_iter()
        .next()
        .expect("u1");
    chat.root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "app.redact".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: Some(vec![ContextEdit::Replace {
                        target: u1.id,
                        messages: vec![user_msg("REDACTED")],
                    }]),
                },
            },
            context(),
        )
        .await
        .expect("submit write");
    let captured_messages = Arc::new(Mutex::new(Vec::new()));
    let cap_cb = Arc::clone(&captured_messages);
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |compaction, _, _| {
                let cap_cb = Arc::clone(&cap_cb);
                Box::pin(async move {
                    *cap_cb.lock().expect("lock") = compaction.messages;
                    Ok(None)
                })
            })),
        },
        None,
    )
    .expect("hook");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert_eq!(user_text(captured_messages.lock().expect("lock").first()), "REDACTED");
    assert!(user_text(chat.faux.summary_reqs()[0].messages.get(1)).contains("[User]: REDACTED"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn compacts_a_fork_whose_cut_falls_on_a_parent_entry() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let entries = all_entries(&chat.root, None).await.expect("entries");
    let last_id = entries.last().expect("last").id;
    let fork = chat
        .root
        .fork(
            last_id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let outcome = task_result(&chat, fork.compact(None, context()).await.expect("compact")).await;
    assert_eq!(
        chat.harness
            .submission(compaction_sub_id(&outcome), context())
            .await
            .expect("sub")
            .expect("exists")
            .wait(context())
            .await
            .expect("wait")
            .record
            .status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let u3 = entries
        .iter()
        .find(|r| user_text(r.model.as_ref().and_then(|m| m.first())).starts_with("u3"))
        .expect("u3");
    let view = fork.context(context()).await.expect("ctx");
    let head = view.head.as_ref().expect("head");
    assert_eq!(head.kind, "pi.compaction");
    assert_eq!(head.head, Some(u3.id));
    assert_eq!(head.conversation_id, fork.id());
    let tail_texts: Vec<String> = view.messages[1..].iter().map(|m| user_text(Some(m))).collect();
    assert_eq!(tail_texts, vec![tok_text("u3", 100), tok_text("a3", 100)]);
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    let state = fork.view_state(context()).await.expect("viewState");
    assert_eq!(state.value().entries, view.entries);
    state.dispose().expect("dispose");
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn settles_stale_in_a_fork_reset_while_it_summarizes() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let entries = all_entries(&chat.root, None).await.expect("entries");
    let fork = chat
        .root
        .fork(
            entries.last().expect("last").id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("fork");
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = fork.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    fork.reset(None, context()).await.expect("reset");
    gate.resolve(());
    let outcome = task_result(&chat, id).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    match sub.status(context()).await.expect("status").state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "stale"),
        other => panic!("Expected stale, got {other:?}"),
    }
    assert_eq!(
        fork.context(context())
            .await
            .expect("ctx")
            .head
            .as_ref()
            .map(|h| h.kind.as_str()),
        Some("pi.reset")
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_an_older_queued_summary_and_the_current_one_together_when_idle_admission_drains_the_inbox() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(gate.clone(), failure("bad request"), Some(reached.clone())));
    let failed = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("fails".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("OLDER")));
    let older = task_result(&chat, chat.root.compact(None, context()).await.expect("older")).await;
    gate.resolve(());
    failed.wait(context()).await.expect("wait");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("CURRENT")));
    let current = task_result(&chat, chat.root.compact(None, context()).await.expect("current")).await;
    let mut statuses = Vec::new();
    for outcome in [&older, &current] {
        let sub = chat
            .harness
            .submission(compaction_sub_id(outcome), context())
            .await
            .expect("sub")
            .expect("exists");
        statuses.push(sub.status(context()).await.expect("status").status());
    }
    assert_eq!(
        statuses,
        vec![
            pi_durable::types::SubmissionStatus::Done,
            pi_durable::types::SubmissionStatus::Done
        ]
    );
    assert!(user_text(chat.root.context(context()).await.expect("ctx").messages.first()).contains("CURRENT"));
    chat.harness.close(context()).await.expect("close");
}

fn define_child_task(gate: Deferred<()>) -> Task<(), serde_json::Value, Option<()>, ()> {
    let mut phases = IndexMap::new();
    let gate_run = gate.clone();
    phases.insert(
        "run".to_owned(),
        Arc::new(
            move |_task: pi_durable::types::RunningTask<(), serde_json::Value, Option<()>>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), serde_json::Value, Option<()>, ()>,
                  task_ctx: Context| {
                let gate_run = gate_run.clone();
                Box::pin(async move {
                    gate_run.wait().await?;
                    runtime
                        .commit(
                            |_tx, _current| {
                                Box::pin(async {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Completed { result: None },
                                    }))
                                })
                            },
                            task_ctx,
                        )
                        .await
                }) as pi_js::BoxFuture<_>
            },
        ) as pi_durable::tasks::TypedPhaseHandler<(), serde_json::Value, Option<()>, ()>,
    );
    define_task(TaskDefinition {
        name: "test.child".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "run" }))),
        phases,
        abort: Arc::new(|_, runtime, task_ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        task_ctx,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

#[tokio::test]
async fn places_a_hooks_summary_and_holds_while_work_the_hook_created_runs() {
    let chat = open_chat_env(None, None, None, None).await;
    let child_gate = deferred::<()>();
    let child = define_child_task(child_gate.clone());
    let child_ref = child.clone();
    add_task(&chat.setup.registry, child.into_any(), None).expect("add child task");
    let harness_cb = chat.harness.clone();
    let root_id = chat.root.id();
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |_, api, hook_ctx| {
                let harness_cb = harness_cb.clone();
                let child_ref = child_ref.clone();
                Box::pin(async move {
                    let task_id = api.task_id();
                    harness_cb
                        .session()
                        .commit(
                            move |tx| async move {
                                tx.create_task(
                                    &child_ref,
                                    &(),
                                    TaskOptions {
                                        ownership: TaskOwnership::Task { task_id },
                                        conversation_id: Some(root_id),
                                        background: false,
                                    },
                                )
                                .await?;
                                Ok(())
                            },
                            hook_ctx,
                        )
                        .await?;
                    Ok(Some(BeforeCompactDecision::Summary("HOOK".to_owned())))
                })
            })),
        },
        None,
    )
    .expect("hook");
    history(&chat).await;
    let id = chat.root.compact(None, context()).await.expect("compact");
    wait_for(
        || {
            let h = chat.harness.clone();
            async move {
                h.get_task(id, context())
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|t| t.state.status() == pi_durable::types::TaskStatus::Completing)
            }
        },
        5_000,
    )
    .await;
    assert_eq!(
        entry_kinds(&chat.root).await.last().map(String::as_str),
        Some("pi.compaction")
    );
    assert!(live_doc(&chat).await.compactions.is_none());
    child_gate.resolve(());
    assert!(matches!(task_result(&chat, id).await, TaskOutcome::Completed { .. }));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn removes_the_status_of_a_faulted_compaction() {
    let storage = Arc::new(ControlledStorage::new());
    let chat = open_chat_env(None, None, Some(storage.clone() as Arc<dyn Storage>), None).await;
    history(&chat).await;
    // Reject the next commit immediately when phase_select finishes beforeCompact so the scheduler faults the compaction.
    let storage_cb = Arc::clone(&storage);
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |_, _, _| {
                storage_cb.fail_next_commit(StorageRejected::new("models broke").into_error());
                Box::pin(async { Ok(None) })
            })),
        },
        None,
    )
    .expect("hook");
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    match outcome {
        TaskOutcome::Faulted { error } => assert!(error.message.contains("models broke")),
        other => panic!("Expected Faulted, got {other:?}"),
    }
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

// ─── Compaction events and live status ────────────────────────────────────

#[tokio::test]
async fn reports_start_and_end_and_the_retry_backoff_in_a_late_joiners_snapshot() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(60_000),
            max_agent_delay_ms: None,
        });
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_cb = Arc::clone(&events);
    let stream = chat.root.watch_events(context()).await.expect("watch");
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().expect("lock").extend(batch);
                Ok(())
            })
        })
        .expect("start");
    chat.faux.push_summary(ScriptStep::Message(failure("overloaded")));
    let id = chat.root.compact(None, context()).await.expect("compact");
    wait_for(
        || {
            let h = chat.harness.clone();
            let root_id = chat.root.id();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|l| l.compactions)
                    .and_then(|c| c.into_iter().next())
                    .is_some_and(|c| c.retry.is_some())
            }
        },
        5_000,
    )
    .await;
    let late = chat.root.watch_events(context()).await.expect("late watch");
    assert_eq!(late.snapshot.compactions.len(), 1);
    let snap_c = &late.snapshot.compactions[0];
    assert_eq!(snap_c.task_id, id);
    assert_eq!(snap_c.reason, CompactionReason::Manual);
    assert!(!snap_c.blocking);
    assert_eq!(snap_c.attempt, 1);
    assert_eq!(snap_c.retry.as_ref().map(|r| r.error.as_str()), Some("overloaded"));
    late.stop().await;
    chat.harness.abort_task(id.erased(), context()).await.expect("abort");
    wait_for(
        || {
            let events = Arc::clone(&events);
            async move {
                events
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|e| matches!(e, AgentEvent::CompactionEnd { .. }))
            }
        },
        5_000,
    )
    .await;
    let comp_events: Vec<AgentEvent> = events
        .lock()
        .expect("lock")
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionStart { .. } | AgentEvent::CompactionEnd { .. }))
        .cloned()
        .collect();
    assert_eq!(
        comp_events,
        vec![
            AgentEvent::CompactionStart {
                task_id: id,
                reason: CompactionReason::Manual,
                blocking: false,
            },
            AgentEvent::CompactionEnd {
                task_id: id,
                reason: CompactionReason::Manual,
            },
        ]
    );
    stream.stop().await;
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lists_concurrent_compactions_in_task_id_order() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let first = deferred::<()>();
    let second = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(first.clone())));
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(second.clone())));
    let a = chat.root.compact(None, context()).await.expect("a");
    let b = chat.root.compact(None, context()).await.expect("b");
    tokio::try_join!(first.wait(), second.wait()).expect("both reached");
    let ids: Option<Vec<TaskId<CompactionResult>>> = live_doc(&chat)
        .await
        .compactions
        .map(|list| list.into_iter().map(|s| s.task_id).collect());
    assert_eq!(ids, Some(vec![a, b]));
    chat.root.abort(context(), None).await.expect("abort");
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

// ─── Recovery ─────────────────────────────────────────────────────────────

async fn reopen_sqlite(path: &str, setup: &ChatSetup, faux: &Script) -> Chat {
    let store = open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
        .await
        .expect("open sqlite");
    let opened = open_chat(Arc::new(store), setup, OpenChatEnv::None)
        .await
        .expect("reopen");
    opened.harness.resume().expect("resume");
    Chat {
        harness: opened.harness,
        root: opened.root,
        setup: setup.clone(),
        faux: faux.clone(),
    }
}

async fn first_sqlite(path: &str) -> Chat {
    let setup = chat_setup(None);
    let faux = attach_script(&setup);
    add_section(
        &setup.registry,
        "preamble",
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are helpful.".to_owned())) })),
        Some(pi_durable::harness::define::SectionOptions { tag: Some(false) }),
        None,
    )
    .expect("preamble");
    let store = open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
        .await
        .expect("open sqlite");
    let opened = open_chat(Arc::new(store), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    setup.settings.update(|s| {
        s.compaction = Some(partial_from(manual_policy()));
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(1),
            max_agent_delay_ms: None,
        });
    });
    opened.harness.resume().expect("resume");
    let chat = Chat {
        harness: opened.harness,
        root: opened.root,
        setup,
        faux,
    };
    history(&chat).await;
    chat
}

#[tokio::test]
async fn repeats_nothing_after_reopen_once_the_summary_is_placed() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let entries = all_entries(&chat.root, None).await.expect("entries");
    let usage = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage");
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.harness.wait_for_idle(context()).await.expect("idle");
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    assert_eq!(all_entries(&chat.root, None).await.expect("entries"), entries);
    assert_eq!(
        chat.harness
            .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
            .await
            .expect("usage"),
        usage
    );
    let inspection = chat.harness.inspect(context()).await.expect("inspect");
    assert!(inspection.tasks.is_empty());
    assert!(inspection.submissions.is_empty());
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_an_overflow_run_with_its_text_when_its_compaction_fails_after_reopen() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let generation = live_doc(&chat).await.run.expect("run").task_id;
    let gen_task = chat
        .harness
        .get_erased_task(generation, context())
        .await
        .expect("gen task")
        .expect("exists");
    match &gen_task.state {
        TaskState::Waiting { checkpoint, .. } => {
            assert_eq!(checkpoint.get("phase").and_then(|v| v.as_str()), Some("prepare"));
            assert_eq!(checkpoint.get("overflow").and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected waiting generation, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.faux.push_summary(ScriptStep::Message(failure("bad request")));
    let settled = chat
        .harness
        .submission(input.id(), context())
        .await
        .expect("sub")
        .expect("exists")
        .wait(context())
        .await
        .expect("wait");
    match settled.record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
            assert_eq!(reason, "model_error");
            assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
        }
        other => panic!("Expected Unanswered model_error, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reruns_select_and_its_hook_after_a_crash_in_select() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let reached = deferred::<()>();
    let reached_cb = reached.clone();
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |_, _, hook_ctx| {
                let n = calls_cb.fetch_add(1, Ordering::AcqRel) + 1;
                let reached_cb = reached_cb.clone();
                Box::pin(async move {
                    if n == 1 {
                        reached_cb.resolve(());
                        if let Some(sig) = hook_ctx.abort_signal() {
                            return aborted(&sig).await;
                        }
                    }
                    Ok(None)
                })
            })),
        },
        None,
    )
    .expect("hook");
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    assert!(matches!(task_result(&chat, id).await, TaskOutcome::Completed { .. }));
    assert_eq!(calls.load(Ordering::Acquire), 2);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resends_an_interrupted_summarization_once_and_counts_only_the_answered_attempt() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    let usage_before = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("doc")
        .models["faux/faux-1"]
        .clone();
    let task = chat
        .harness
        .get_task(id, context())
        .await
        .expect("get_task")
        .expect("exists");
    assert!(match &task.state {
        TaskState::Running { checkpoint } => matches!(
            serde_json::from_value::<pi_durable::harness::compaction::CompactionCheckpoint>(checkpoint.clone()),
            Ok(pi_durable::harness::compaction::CompactionCheckpoint::Summarize { .. })
        ),
        _ => false,
    });
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    assert!(matches!(task_result(&chat, id).await, TaskOutcome::Completed { .. }));
    let sum_reqs = chat.faux.summary_reqs();
    assert_eq!(sum_reqs.len(), 2);
    let normalize_timestamps = |msgs: &[Message]| -> Vec<Message> {
        msgs.iter()
            .map(|m| match m {
                Message::User(u) => {
                    let mut u = u.clone();
                    u.timestamp = 0;
                    Message::User(u)
                }
                Message::System(s) => {
                    let mut s = s.clone();
                    s.timestamp = 0;
                    Message::System(s)
                }
                other => other.clone(),
            })
            .collect()
    };
    assert_eq!(
        normalize_timestamps(&sum_reqs[1].messages),
        normalize_timestamps(&sum_reqs[0].messages)
    );
    let usage = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("doc")
        .models["faux/faux-1"]
        .clone();
    assert!(usage.input > usage_before.input);
    assert_eq!(usage.output - usage_before.output, 2);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resumes_a_retry_backoff_after_reopen() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    let now_val = Arc::new(AtomicI64::new(pi_js::time::now_ms()));
    let now_cb = Arc::clone(&now_val);
    chat.setup.now.set(move || now_cb.load(Ordering::Acquire));
    chat.setup.settings.update(|s| {
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(60_000),
            max_agent_delay_ms: None,
        });
    });
    chat.faux.push_summary(ScriptStep::Message(failure("overloaded")));
    let id = chat.root.compact(None, context()).await.expect("compact");
    wait_for(
        || {
            let h = chat.harness.clone();
            let root_id = chat.root.id();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|l| l.compactions)
                    .and_then(|c| c.into_iter().next())
                    .is_some_and(|c| c.retry.is_some())
            }
        },
        5_000,
    )
    .await;
    let task = chat
        .harness
        .get_task(id, context())
        .await
        .expect("get_task")
        .expect("exists");
    match &task.state {
        TaskState::Running { checkpoint } => {
            match serde_json::from_value::<pi_durable::harness::compaction::CompactionCheckpoint>(checkpoint.clone()) {
                Ok(pi_durable::harness::compaction::CompactionCheckpoint::Retry { request, .. }) => {
                    assert_eq!(request.attempt, 1);
                }
                other => panic!("Expected Retry checkpoint, got {other:?}"),
            }
        }
        other => panic!("Expected Retry checkpoint, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
    now_val.fetch_add(120_000, Ordering::AcqRel);
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    assert!(matches!(task_result(&chat, id).await, TaskOutcome::Completed { .. }));
    assert_eq!(chat.faux.summary_reqs().len(), 2);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_generation_waiting_on_its_blocking_compaction_across_reopen() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    let setup = chat.setup.clone();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let mut pol = blocking_policy();
    pol.reserve_tokens = 128_000 - 700;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &setup, &chat.faux).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    assert_eq!(
        chat.harness
            .submission(input.id(), context())
            .await
            .expect("sub")
            .expect("exists")
            .wait(context())
            .await
            .expect("wait")
            .record
            .status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let k = entry_kinds(&chat.root).await;
    assert_eq!(&k[k.len() - 3..], &["pi.compaction", "pi.system", "pi.assistant"]);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_queued_summary_across_reopen_and_places_it_at_the_next_boundary() {
    let (_dir, path) = sqlite_path().await;
    let mut chat = first_sqlite(&path).await;
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(deferred(), answer("never"), Some(reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    chat.harness.close(context()).await.expect("close");
    chat = reopen_sqlite(&path, &chat.setup, &chat.faux).await;
    chat.faux.push_agent(ScriptStep::Message(answer("answered")));
    assert_eq!(
        chat.harness
            .submission(input.id(), context())
            .await
            .expect("sub")
            .expect("exists")
            .wait(context())
            .await
            .expect("wait")
            .record
            .status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let sub_id = compaction_sub_id(&outcome);
    assert_eq!(
        chat.harness
            .submission(sub_id, context())
            .await
            .expect("sub")
            .expect("exists")
            .wait(context())
            .await
            .expect("wait")
            .record
            .status(),
        pi_durable::types::SubmissionStatus::Done
    );
    chat.harness.close(context()).await.expect("close");
}

// ─── Compaction and the inbox ─────────────────────────────────────────────

async fn queued_summary_sub(chat: &Chat, content: &str) -> pi_durable::harness::submissions::SubmissionHandle {
    chat.faux.push_summary(ScriptStep::Message(summary_msg(content)));
    let outcome = task_result(chat, chat.root.compact(None, context()).await.expect("compact")).await;
    chat.harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists")
}

async fn start_busy(
    chat: &Chat,
    reply: AssistantMessage,
) -> (pi_durable::harness::submissions::SubmissionHandle, Deferred<()>) {
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux.push_agent(gated(gate.clone(), reply, Some(reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    (input, gate)
}

#[tokio::test]
async fn places_a_reset_queued_after_the_summary_last_and_makes_a_summary_queued_after_a_reset_stale() {
    for summary_first in [true, false] {
        let chat = open_chat_env(None, None, None, None).await;
        history(&chat).await;
        let (input, gate) = start_busy(&chat, answer("done")).await;
        let submission = if summary_first {
            let sub = queued_summary_sub(&chat, "SUMMARY").await;
            chat.root.reset(None, context()).await.expect("reset");
            sub
        } else {
            chat.root.reset(None, context()).await.expect("reset");
            queued_summary_sub(&chat, "SUMMARY").await
        };
        gate.resolve(());
        input.wait(context()).await.expect("wait");
        let settled = submission.wait(context()).await.expect("wait sub");
        assert_eq!(
            settled.record.status(),
            if summary_first {
                pi_durable::types::SubmissionStatus::Done
            } else {
                pi_durable::types::SubmissionStatus::Unanswered
            }
        );
        assert_eq!(
            entry_kinds(&chat.root).await.last().map(String::as_str),
            Some("pi.reset")
        );
        assert_eq!(
            chat.root
                .context(context())
                .await
                .expect("ctx")
                .head
                .as_ref()
                .map(|h| h.kind.as_str()),
            Some("pi.reset")
        );
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn places_a_summary_left_queued_by_a_failed_run_at_the_next_submission_before_its_input() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let (input, gate) = start_busy(&chat, failure("bad request")).await;
    let submission = queued_summary_sub(&chat, "SUMMARY").await;
    gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Unanswered
    );
    assert_eq!(
        submission.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    turn(&chat, "again", "ok").await;
    assert_eq!(
        submission.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let agent_reqs = chat.faux.agent_reqs();
    let request = &agent_reqs.last().expect("req").messages;
    assert!(user_text(request.first()).contains("SUMMARY"));
    assert!(request.iter().any(|m| user_text(Some(m)) == "again"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_the_full_retry_budget_after_an_overflow_compaction() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    chat.faux.push_agent(ScriptStep::Message(failure("prompt is too long")));
    chat.faux.push_agent(ScriptStep::Message(failure("overloaded")));
    chat.faux.push_agent(ScriptStep::Message(failure("overloaded")));
    chat.faux.push_agent(ScriptStep::Message(answer("finally")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn loses_an_application_edit_placed_while_a_compaction_summarizes() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let u1 = all_entries(&chat.root, None)
        .await
        .expect("entries")
        .into_iter()
        .next()
        .expect("u1");
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    let edit = ContextEdit::Replace {
        target: u1.id,
        messages: vec![user_msg("REDACTED")],
    };
    chat.root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "app.redact".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: Some(vec![edit]),
                },
            },
            context(),
        )
        .await
        .expect("write");
    gate.resolve(());
    task_result(&chat, id).await;
    assert!(user_text(chat.faux.summary_reqs()[0].messages.get(1)).contains("[User]: u1 "));
    let texts: Vec<String> = chat
        .root
        .context(context())
        .await
        .expect("ctx")
        .messages
        .iter()
        .map(|m| user_text(Some(m)))
        .collect();
    assert!(!texts.iter().any(|t| t == "REDACTED"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_the_kept_entries_mounted_in_the_conversation_view() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let state = chat.root.view_state(context()).await.expect("viewState");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    wait_for(
        || {
            let st = state.clone();
            async move { st.value().entries.first().is_some_and(|e| e.kind == "pi.compaction") }
        },
        5_000,
    )
    .await;
    assert_eq!(
        state.value().entries,
        chat.root.context(context()).await.expect("ctx").entries
    );
    state.dispose().expect("dispose");
    chat.harness.close(context()).await.expect("close");
}

// ─── Compaction edge cases ────────────────────────────────────────────────

#[tokio::test]
async fn keeps_the_one_compaction_limit_through_a_retry_backoff() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    let asked = Arc::new(AtomicUsize::new(0));
    let asked_cb = Arc::clone(&asked);
    add_hooks(
        &chat.setup.registry,
        &COMPACTION_TASK,
        CompactionHooks {
            before_compact: Some(Arc::new(move |_, _, _| {
                asked_cb.fetch_add(1, Ordering::AcqRel);
                Box::pin(async { Ok(Some(BeforeCompactDecision::Decline)) })
            })),
        },
        None,
    )
    .expect("hook");
    chat.faux.push_agent(ScriptStep::Message(failure("overloaded")));
    turn(&chat, &tok_text("u4", 200), "a4").await;
    assert_eq!(asked.load(Ordering::Acquire), 1);
    assert_eq!(chat.faux.agent_reqs().len(), 5);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_start_a_background_compaction_when_a_manual_one_was_admitted_during_preparation() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(background_policy())));
    let rendering = deferred::<()>();
    let release = deferred::<()>();
    let hold = Arc::new(AtomicBool::new(true));
    let rendering_cb = rendering.clone();
    let release_cb = release.clone();
    add_section(
        &chat.setup.registry,
        "slow",
        Arc::new(move |_, _| {
            let hold = Arc::clone(&hold);
            let rendering_cb = rendering_cb.clone();
            let release_cb = release_cb.clone();
            Box::pin(async move {
                if hold.swap(false, Ordering::AcqRel) {
                    rendering_cb.resolve(());
                    release_cb.wait().await?;
                }
                Ok(Some("slow".to_owned()))
            })
        }),
        None,
        None,
    )
    .expect("slow section");
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    rendering.wait().await.expect("rendering");
    let manual = chat.root.compact(None, context()).await.expect("manual");
    reached.wait().await.expect("reached");
    release.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let ids: Vec<TaskId> = compaction_tasks(&chat).await.into_iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![manual.erased()]);
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    chat.harness
        .abort_task(manual.erased(), context())
        .await
        .expect("abort");
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn starts_no_background_compaction_after_a_blocking_one_in_the_same_generation() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    let mut pol = background_policy();
    pol.background_tokens = 1300;
    pol.keep_recent_tokens = 400;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    turn(&chat, &tok_text("u4", 1000), "a4").await;
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    assert!(compaction_tasks(&chat).await.is_empty());
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sends_the_request_anyway_when_its_blocking_compaction_faults() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    chat.faux.push_summary(ScriptStep::Factory(Arc::new(move |_| {
        Box::pin(async { Err(pi_js::Error::msg("no credentials")) })
    })));
    turn(&chat, &tok_text("u4", 200), "a4").await;
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_an_overflow_run_with_its_text_when_its_compaction_is_aborted_directly_or_itself_overflows() {
    for abort_how in [true, false] {
        let chat = open_chat_env(None, None, None, None).await;
        history(&chat).await;
        chat.setup
            .settings
            .update(|s| s.compaction = Some(partial_from(enabled_manual())));
        let reached = deferred::<()>();
        if abort_how {
            chat.faux
                .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
        } else {
            chat.faux
                .push_summary(ScriptStep::Message(failure("prompt is too long for the summary")));
        }
        chat.faux.push_agent(ScriptStep::Message(failure(OVERFLOW)));
        let input = chat
            .root
            .submit(
                SubmissionDraft::Input {
                    request_id: None,
                    content: UserInput::Text(tok_text("u4", 100)),
                    when_busy: None,
                },
                context(),
            )
            .await
            .expect("submit");
        if abort_how {
            reached.wait().await.expect("reached");
            let child = compaction_tasks(&chat).await.into_iter().next().expect("child");
            chat.harness.abort_task(child.id, context()).await.expect("abort");
        }
        match input.wait(context()).await.expect("wait").record.state {
            SubmissionState::Input(InputSubmissionState::Unanswered { reason, detail, .. }) => {
                assert_eq!(reason, "model_error");
                assert_eq!(detail.as_ref().and_then(|v| v.as_str()), Some(OVERFLOW));
            }
            other => panic!("Expected model_error, got {other:?}"),
        }
        assert!(live_doc(&chat).await.compactions.is_none());
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn treats_a_length_stop_as_an_ordinary_answer_not_an_overflow() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(enabled_manual())));
    chat.faux.push_agent(ScriptStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("cut short".to_owned()),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            error_message: Some(OVERFLOW.to_owned()),
            ..Default::default()
        }),
    )));
    let input = chat
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
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orders_the_events_of_a_summary_placed_at_once() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_cb = Arc::clone(&events);
    let stream = chat.root.watch_events(context()).await.expect("watch");
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().expect("lock").extend(batch);
                Ok(())
            })
        })
        .expect("start");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    wait_for(
        || {
            let events = Arc::clone(&events);
            async move {
                events
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|e| matches!(e, AgentEvent::CompactionEnd { .. }))
            }
        },
        5_000,
    )
    .await;
    let list = events.lock().expect("lock");
    let end = list
        .iter()
        .position(|e| matches!(e, AgentEvent::CompactionEnd { .. }))
        .expect("compaction_end");
    let slice: Vec<&str> = list[end - 2..end + 3]
        .iter()
        .map(|e| match e {
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::CompactionEnd { .. } => "compaction_end",
            AgentEvent::Submission { .. } => "submission",
            AgentEvent::UsageChanged { .. } => "usage_changed",
            _ => "other",
        })
        .collect();
    assert_eq!(
        slice,
        vec![
            "message_start",
            "message_end",
            "compaction_end",
            "submission",
            "usage_changed",
        ]
    );
    drop(list);
    stream.stop().await;
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn puts_compaction_start_last_in_the_batch_of_the_preparation_commit() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(background_policy())));
    let batches = Arc::new(Mutex::new(Vec::<Vec<AgentEvent>>::new()));
    let batches_cb = Arc::clone(&batches);
    let stream = chat.root.watch_events(context()).await.expect("watch");
    stream
        .start(move |batch, _| {
            let batches_cb = Arc::clone(&batches_cb);
            Box::pin(async move {
                batches_cb.lock().expect("lock").push(batch);
                Ok(())
            })
        })
        .expect("start");
    add_section(
        &chat.setup.registry,
        "extra",
        Arc::new(|_, _| Box::pin(async { Ok(Some("extra".to_owned())) })),
        None,
        None,
    )
    .expect("extra");
    chat.faux.push_summary(gated(deferred(), summary_msg("SUMMARY"), None));
    turn(&chat, &tok_text("u4", 100), "a4").await;
    wait_for(
        || {
            let batches = Arc::clone(&batches);
            async move {
                batches
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|b| b.iter().any(|e| matches!(e, AgentEvent::CompactionStart { .. })))
            }
        },
        5_000,
    )
    .await;
    let found = batches
        .lock()
        .expect("lock")
        .iter()
        .find(|b| b.iter().any(|e| matches!(e, AgentEvent::CompactionStart { .. })))
        .expect("batch")
        .iter()
        .map(|e| match e {
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::CompactionStart { .. } => "compaction_start",
            _ => "other",
        })
        .collect::<Vec<_>>();
    assert_eq!(found, vec!["message_start", "message_end", "compaction_start"]);
    stream.stop().await;
    let bg_id = compaction_tasks(&chat).await[0].id;
    chat.harness.abort_task(bg_id, context()).await.expect("abort");
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_background_summary_queued_through_a_retry_backoff_while_a_blocking_compaction_wins() {
    let chat = open_chat_env(None, Some(2000), None, None).await;
    history(&chat).await;
    chat.setup.settings.update(|s| {
        s.compaction = Some(partial_from(background_policy()));
        s.retry = Some(PartialRetryPolicy {
            enabled: Some(true),
            max_retries: Some(2),
            base_delay_ms: Some(300),
            max_agent_delay_ms: None,
        });
    });
    let summary_gate = deferred::<()>();
    let summary_reached = deferred::<()>();
    chat.faux.push_summary(gated(
        summary_gate.clone(),
        summary_msg("BACKGROUND"),
        Some(summary_reached.clone()),
    ));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("BLOCKING")));
    let failed = deferred::<()>();
    let failed_cb = failed.clone();
    chat.faux.push_agent(ScriptStep::Factory(Arc::new(move |_| {
        failed_cb.resolve(());
        Box::pin(async { Ok(failure("overloaded")) })
    })));
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 100)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    tokio::try_join!(failed.wait(), summary_reached.wait()).expect("both");
    wait_for(
        || {
            let h = chat.harness.clone();
            let root_id = chat.root.id();
            async move {
                h.snapshot_conversation(&*LIVE_DOC, root_id, context())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|l| l.generation)
                    .is_some_and(|g| g.retry.is_some())
            }
        },
        5_000,
    )
    .await;
    let background = compaction_tasks(&chat).await.into_iter().next().expect("bg");
    summary_gate.resolve(());
    let queued = task_result(&chat, TaskId::new_unchecked(background.id.as_i64())).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&queued), context())
        .await
        .expect("sub")
        .expect("exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    let mut pol = background_policy();
    pol.reserve_tokens = 1500;
    pol.keep_recent_tokens = 50;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert!(user_text(chat.faux.agent_reqs().last().expect("req").messages.first()).contains("BLOCKING"));
    match sub.wait(context()).await.expect("wait").record.state {
        SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "stale"),
        other => panic!("Expected stale, got {other:?}"),
    }
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn summarizes_the_previous_summary_in_a_second_compaction() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("FIRST")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("c1")).await;
    turn(&chat, &tok_text("u4", 100), &tok_text("a4", 100)).await;
    turn(&chat, &tok_text("u5", 100), &tok_text("a5", 100)).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SECOND")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("c2")).await;
    let prompt = user_text(chat.faux.summary_reqs()[1].messages.get(1));
    assert!(prompt.starts_with("<conversation>\n[User]: The conversation history before this point was compacted"));
    assert!(prompt.contains("FIRST"));
    assert!(user_text(chat.root.context(context()).await.expect("ctx").messages.first()).contains("SECOND"));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn leaves_no_usage_submission_summary_or_outcome_when_the_classifying_commit_is_rejected() {
    let storage = Arc::new(ControlledStorage::new());
    let chat = open_chat_env(None, None, Some(storage.clone() as Arc<dyn Storage>), None).await;
    history(&chat).await;
    let usage_before = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("doc")
        .models
        .get("faux/faux-1")
        .cloned();
    let storage_cb = Arc::clone(&storage);
    chat.faux.push_summary(ScriptStep::Factory(Arc::new(move |_| {
        storage_cb.fail_next_commit(StorageRejected::new("rejected").into());
        Box::pin(async { Ok(summary_msg("SUMMARY")) })
    })));
    let id = chat.root.compact(None, context()).await.expect("compact");
    let outcome = task_result(&chat, id).await;
    assert!(matches!(outcome, TaskOutcome::Faulted { .. }));
    let usage_after = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("doc")
        .models
        .get("faux/faux-1")
        .cloned();
    assert_eq!(usage_after, usage_before);
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    assert!(
        chat.harness
            .inspect(context())
            .await
            .expect("inspect")
            .submissions
            .is_empty()
    );
    let found = storage
        .submission_by_request(chat.root.id(), &format!("compaction:{id}"), context())
        .await
        .expect("lookup");
    assert!(found.is_none());
    assert!(live_doc(&chat).await.compactions.is_none());
    chat.harness.close(context()).await.expect("close");
}

// ─── Blocking and manual compaction together ──────────────────────────────

async fn start_blocking_run(
    chat: &Chat,
) -> (
    pi_durable::harness::submissions::SubmissionHandle,
    Deferred<()>,
    TaskId<CompactionResult>,
) {
    history(chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    let gate = deferred::<()>();
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(gate.clone(), summary_msg("BLOCKING"), Some(reached.clone())));
    let input = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text(tok_text("u4", 200)),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    let blocking = compaction_tasks(chat).await.into_iter().next().expect("blocking");
    (input, gate, TaskId::new_unchecked(blocking.id.as_i64()))
}

#[tokio::test]
async fn places_a_manual_summary_selected_before_the_blocking_one_landed_its_equal_cut_replaces_it() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    let (input, gate, _) = start_blocking_run(&chat).await;
    chat.faux.push_summary(ScriptStep::Message(summary_msg("MANUAL")));
    let manual = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&manual), context())
        .await
        .expect("sub")
        .expect("exists");
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    chat.faux.push_agent(ScriptStep::Message(answer("a4")));
    gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    assert!(user_text(chat.faux.agent_reqs().last().expect("req").messages.first()).contains("BLOCKING"));
    assert_eq!(
        sub.wait(context()).await.expect("wait sub").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let markers: Vec<EntryRecord> = all_entries(&chat.root, None)
        .await
        .expect("entries")
        .into_iter()
        .filter(|r| r.kind == "pi.compaction")
        .collect();
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[1].head, markers[0].head);
    let messages = chat.root.context(context()).await.expect("ctx").messages;
    assert!(user_text(messages.first()).contains("MANUAL"));
    assert!(!messages.iter().any(|m| user_text(Some(m)).contains("BLOCKING")));
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn finds_nothing_to_compact_for_a_manual_compaction_selected_after_the_blocking_summary_landed() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    let (input, gate, _) = start_blocking_run(&chat).await;
    let answer_gate = deferred::<()>();
    let answer_reached = deferred::<()>();
    chat.faux
        .push_agent(gated(answer_gate.clone(), answer("a4"), Some(answer_reached.clone())));
    gate.resolve(());
    answer_reached.wait().await.expect("reached");
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert_eq!(
        outcome,
        TaskOutcome::Completed {
            result: CompactionResult {
                entry_id: None,
                submission_id: None,
            }
        }
    );
    assert_eq!(chat.faux.summary_reqs().len(), 1);
    answer_gate.resolve(());
    assert_eq!(
        input.wait(context()).await.expect("wait").record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_the_run_and_both_compactions_on_esc_and_appends_nothing() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    let (input, _gate, blocking) = start_blocking_run(&chat).await;
    let manual_reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("MANUAL"), Some(manual_reached.clone())));
    let manual = chat.root.compact(None, context()).await.expect("compact");
    manual_reached.wait().await.expect("manual reached");
    let flags: Option<Vec<bool>> = live_doc(&chat)
        .await
        .compactions
        .map(|list| list.into_iter().map(|s| s.blocking).collect());
    assert_eq!(flags, Some(vec![true, false]));
    chat.root.abort(context(), None).await.expect("abort");
    match input.wait(context()).await.expect("wait").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("Expected aborted, got {other:?}"),
    }
    assert!(matches!(
        task_result(&chat, blocking).await,
        TaskOutcome::Aborted { .. }
    ));
    assert!(matches!(task_result(&chat, manual).await, TaskOutcome::Aborted { .. }));
    let l = live_doc(&chat).await;
    assert!(l.compactions.is_none());
    assert!(l.run.is_none());
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    assert!(
        chat.harness
            .inspect(context())
            .await
            .expect("inspect")
            .submissions
            .is_empty()
    );
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_a_summary_that_survived_esc_at_the_next_submission_before_its_input() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let reached = deferred::<()>();
    chat.faux
        .push_agent(gated(deferred(), answer("never"), Some(reached.clone())));
    let busy = chat
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("busy".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    reached.wait().await.expect("reached");
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let sub = chat
        .harness
        .submission(compaction_sub_id(&outcome), context())
        .await
        .expect("sub")
        .expect("exists");
    chat.root.abort(context(), None).await.expect("abort");
    match busy.wait(context()).await.expect("wait").record.state {
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) => assert_eq!(reason, "aborted"),
        other => panic!("Expected aborted, got {other:?}"),
    }
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Queued
    );
    turn(&chat, "u5", "a5").await;
    assert_eq!(
        sub.status(context()).await.expect("status").status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let reqs = chat.faux.agent_reqs();
    let request = &reqs.last().expect("req").messages;
    assert!(user_text(request.first()).contains("SUMMARY"));
    assert!(request.iter().any(|m| user_text(Some(m)) == "u5"));
    let k = entry_kinds(&chat.root).await;
    assert_eq!(
        &k[k.len() - 4..],
        &["pi.compaction", "pi.user", "pi.system", "pi.assistant"]
    );
    chat.harness.close(context()).await.expect("close");
}

// ─── Compaction pinning, silent overflow, and late policy changes ─────────

#[tokio::test]
async fn keeps_the_pinned_model_through_a_retry_and_advances_the_live_attempt() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_owned(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: Some(100_000),
                max_tokens: Some(900),
            },
            FauxModelDefinition {
                id: "faux-2".to_owned(),
                name: None,
                reasoning: None,
                input: None,
                input_limits: None,
                cost: None,
                context_window: Some(100_000),
                max_tokens: Some(900),
            },
        ]),
        ..Default::default()
    }));
    let chat = open_chat_env(None, None, None, Some(setup)).await;
    history(&chat).await;
    let attempt = Arc::new(Mutex::new(None));
    let root_cb = chat.root.clone();
    chat.faux.push_summary(ScriptStep::Factory(Arc::new(move |_| {
        let root_cb = root_cb.clone();
        Box::pin(async move {
            root_cb
                .configure(
                    AgentChange {
                        model: FieldChange::Set(ModelRef {
                            provider: "faux".to_owned(),
                            model_id: "faux-2".to_owned(),
                        }),
                        ..Default::default()
                    },
                    context(),
                )
                .await?;
            Ok(failure("overloaded"))
        })
    })));
    let attempt_cb = Arc::clone(&attempt);
    let harness_cb = chat.harness.clone();
    let root_id = chat.root.id();
    chat.faux.push_summary(ScriptStep::Factory(Arc::new(move |_| {
        let harness_cb = harness_cb.clone();
        let attempt_cb = Arc::clone(&attempt_cb);
        Box::pin(async move {
            let live = harness_cb
                .snapshot_conversation(&*LIVE_DOC, root_id, context())
                .await?
                .unwrap_or_default();
            *attempt_cb.lock().expect("lock") = live.compactions.and_then(|c| c.into_iter().next()).map(|c| c.attempt);
            Ok(summary_msg("SUMMARY"))
        })
    })));
    let outcome = task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    let models: Vec<String> = chat.faux.summary_reqs().into_iter().map(|r| r.model).collect();
    assert_eq!(models, vec!["faux-1", "faux-1"]);
    assert_eq!(*attempt.lock().expect("lock"), Some(2));
    let usage = chat
        .harness
        .snapshot_conversation(&*USAGE_DOC, chat.root.id(), context())
        .await
        .expect("usage")
        .expect("doc")
        .models;
    let keys: Vec<&String> = usage.keys().collect();
    assert_eq!(keys, vec!["faux/faux-1"]);
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn shows_a_late_joiner_a_compaction_that_is_summarizing() {
    let chat = open_chat_env(None, None, None, None).await;
    history(&chat).await;
    let reached = deferred::<()>();
    chat.faux
        .push_summary(gated(deferred(), summary_msg("SUMMARY"), Some(reached.clone())));
    let id = chat.root.compact(None, context()).await.expect("compact");
    reached.wait().await.expect("reached");
    let stream = chat.root.watch_events(context()).await.expect("watch");
    assert_eq!(
        stream.snapshot.compactions,
        vec![CompactionStatus {
            task_id: id,
            reason: CompactionReason::Manual,
            blocking: false,
            attempt: 1,
            retry: None,
        }]
    );
    stream.stop().await;
    chat.harness.abort_task(id.erased(), context()).await.expect("abort");
    chat.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn treats_silent_overflow_as_an_ordinary_answer() {
    let responses = [
        answer("fine"),
        faux_assistant_message(
            FauxAssistantMessageContent::Text(String::new()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        ),
    ];
    for response in responses {
        let chat = open_chat_env(None, Some(300), None, None).await;
        history(&chat).await;
        chat.setup.settings.update(|s| {
            s.compaction = Some(PartialCompactionPolicy {
                enabled: Some(true),
                reserve_tokens: Some(-100_000),
                keep_recent_tokens: Some(150),
                background_tokens: Some(0),
            });
        });
        chat.faux.push_agent(ScriptStep::Message(response));
        let input = chat
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
        assert_eq!(
            input.wait(context()).await.expect("wait").record.status(),
            pi_durable::types::SubmissionStatus::Done
        );
        let last = all_entries(&chat.root, None)
            .await
            .expect("entries")
            .pop()
            .expect("last");
        let Some(Message::Assistant(msg)) = last.model.as_ref().and_then(|m| m.first()) else {
            panic!("Expected assistant entry");
        };
        assert!(msg.usage.input >= 300);
        assert_eq!(chat.faux.summary_reqs().len(), 0);
        chat.harness.close(context()).await.expect("close");
    }
}

#[tokio::test]
async fn sends_the_request_when_a_blocking_compaction_finds_nothing_under_a_policy_changed_after_preparation() {
    let chat = open_chat_env(None, Some(1000), None, None).await;
    history(&chat).await;
    chat.setup
        .settings
        .update(|s| s.compaction = Some(partial_from(blocking_policy())));
    let changed = Arc::new(AtomicBool::new(false));
    let settings_cb = Arc::clone(&chat.setup.settings);
    add_section(
        &chat.setup.registry,
        "policy",
        Arc::new(move |_, _| {
            if !changed.swap(true, Ordering::AcqRel) {
                settings_cb.update(|s| {
                    let mut pol = blocking_policy();
                    pol.keep_recent_tokens = 100_000;
                    s.compaction = Some(partial_from(pol));
                });
            }
            Box::pin(async { Ok(Some("p".to_owned())) })
        }),
        None,
        None,
    )
    .expect("policy section");
    turn(&chat, &tok_text("u4", 200), "a4").await;
    assert!(compaction_tasks(&chat).await.is_empty());
    assert_eq!(chat.faux.summary_reqs().len(), 0);
    assert!(!entry_kinds(&chat.root).await.iter().any(|k| k == "pi.compaction"));
    chat.harness.close(context()).await.expect("close");
}

// ─── Blocked compaction ───────────────────────────────────────────────────

#[tokio::test]
async fn survives_reopen_blocked_and_is_orphaned_on_abort_with_its_status_removed() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let store = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("open sqlite");
    let opened = open_chat(Arc::new(store), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    let newer: Task<CompactionInput, serde_json::Value, CompactionResult, CompactionHooks> =
        define_task(TaskDefinition {
            name: "pi.compaction".to_owned(),
            version: 2,
            initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "select" }))),
            phases: IndexMap::new(),
            abort: Arc::new(|_, runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _current| {
                                Box::pin(async {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Aborted {
                                            reason: None,
                                            result: None,
                                        },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        });
    let root_id = opened.root.id();
    let id = opened
        .root
        .commit(
            move |tx| async move {
                let task_id = tx
                    .create_task(
                        &newer,
                        &CompactionInput {
                            reason: CompactionReason::Manual,
                            instructions: None,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let live_draft = tx.doc_conversation(&*LIVE_DOC, root_id).await?;
                live_draft.update(|live| {
                    live.compactions = Some(vec![CompactionStatus {
                        task_id,
                        reason: CompactionReason::Manual,
                        blocking: false,
                        attempt: 1,
                        retry: None,
                    }]);
                })?;
                Ok(task_id)
            },
            context(),
        )
        .await
        .expect("create blocked task");
    opened.harness.close(context()).await.expect("close");

    let store2 = open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
        .await
        .expect("reopen sqlite");
    let opened2 = open_chat(Arc::new(store2), &setup, OpenChatEnv::None)
        .await
        .expect("reopen");
    opened2.harness.resume().expect("resume");
    let chat = Chat {
        harness: opened2.harness,
        root: opened2.root,
        setup: setup.clone(),
        faux: attach_script(&setup),
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_cb = Arc::clone(&events);
    let stream = chat.root.watch_events(context()).await.expect("watch");
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().expect("lock").extend(batch);
                Ok(())
            })
        })
        .expect("start");
    let inspection = chat.harness.inspect(context()).await.expect("inspect");
    let found = inspection
        .tasks
        .into_iter()
        .find(|t| t.record.id == id.erased())
        .expect("found task");
    match found.state {
        TaskInspectionState::Blocked { reason, .. } => assert_eq!(reason, TaskBlockedReason::TaskTooOld),
        other => panic!("Expected Blocked(TaskTooOld), got {other:?}"),
    }
    chat.harness.abort_task(id.erased(), context()).await.expect("abort");
    let settled = chat.harness.wait_for_task(id, context()).await.expect("wait");
    match settled.outcome {
        TaskOutcome::Orphaned { reason } => assert_eq!(reason, "task_too_old"),
        other => panic!("Expected Orphaned, got {other:?}"),
    }
    assert!(live_doc(&chat).await.compactions.is_none());
    wait_for(
        || {
            let events = Arc::clone(&events);
            async move {
                events
                    .lock()
                    .expect("lock")
                    .iter()
                    .any(|e| matches!(e, AgentEvent::CompactionEnd { .. }))
            }
        },
        5_000,
    )
    .await;
    stream.stop().await;
    chat.harness.close(context()).await.expect("close");
}

// ─── Context contributions ────────────────────────────────────────────────

#[tokio::test]
async fn apply_edits_carried_by_an_older_head_marker_in_the_range() {
    let chat = open_chat_env(None, None, None, None).await;
    let root_id = chat.root.id();
    let (a_id, b_id, c_id) = chat
        .root
        .commit(
            move |tx| async move {
                let a = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "app.note".to_owned(),
                            model: Some(vec![user_msg("a")]),
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                let b = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "app.note".to_owned(),
                            model: Some(vec![user_msg("b")]),
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "app.head".to_owned(),
                        model: None,
                        data: None,
                        head: Some(EntryDraftHead::Entry(a.id)),
                        edits: Some(vec![ContextEdit::Omit { target: b.id }]),
                    },
                )
                .await?;
                let c = tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "app.note".to_owned(),
                            model: Some(vec![user_msg("c")]),
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await?;
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "app.head".to_owned(),
                        model: Some(vec![user_msg("H")]),
                        data: None,
                        head: Some(EntryDraftHead::Entry(a.id)),
                        edits: None,
                    },
                )
                .await?;
                Ok((a.id, b.id, c.id))
            },
            context(),
        )
        .await
        .expect("commit");
    let view = chat.root.context(context()).await.expect("ctx");
    let ids: Vec<EntryId> = view.entries[1..].iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![a_id, b_id, c_id]);
    let contrib_texts: Vec<Vec<String>> = view
        .contributions
        .iter()
        .map(|msgs| msgs.iter().map(|m| user_text(Some(m))).collect())
        .collect();
    assert_eq!(contrib_texts, vec![vec!["H"], vec!["a"], Vec::<&str>::new(), vec!["c"]]);
    let msg_texts: Vec<String> = view.messages.iter().map(|m| user_text(Some(m))).collect();
    assert_eq!(msg_texts, vec!["H", "a", "c"]);

    let mut pol = manual_policy();
    pol.keep_recent_tokens = 1;
    chat.setup.settings.update(|s| s.compaction = Some(partial_from(pol)));
    chat.faux.push_summary(ScriptStep::Message(summary_msg("SUMMARY")));
    task_result(&chat, chat.root.compact(None, context()).await.expect("compact")).await;
    let prompt = user_text(chat.faux.summary_reqs()[0].messages.get(1));
    assert!(prompt.contains("<conversation>\n[User]: H\n\n[User]: a\n</conversation>"));
    chat.harness.close(context()).await.expect("close");
}

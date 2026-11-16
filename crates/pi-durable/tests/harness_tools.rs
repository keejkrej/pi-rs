//! Port of packages/durable/test/harness-tools.test.ts

mod support;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxDeferredOptions, FauxResponseStep,
    FauxToolCallOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{
    AssistantMessage, ImageContent, Message, StopReason, TextContent, ToolCall, ToolResultMessage, UserContentBlock,
};
use pi_chord::Context;
use pi_durable::entries::{TOOL_RESULT_ENTRY, ToolResultEntryData};
use pi_durable::env::ExecutionEnv;
use pi_durable::env::node::NodeExecutionEnv;
use pi_durable::harness::agent::AGENT_DOC;
use pi_durable::harness::define::{define_extension, define_tool, hook, section};
use pi_durable::harness::generation::{GENERATION_TASK, GenerationHooks};
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::live::{LIVE_DOC, LiveState};
use pi_durable::harness::registry::Registry;
use pi_durable::harness::tool::{TOOL_TASK, ToolHooks};
use pi_durable::harness::types::{
    AgentChange, AgentExtensionsChange, AgentToolsChange, AgentToolsFilter, BeforeToolDecision,
    ConversationCreateOptions, ConversationOwnership, DiagnosticSeverity, DocumentReaderExt, Extension, FieldChange,
    HarnessOptions, ModelRef, OutputRetainMode, StreamDeferredOption, SubmissionDraft, ToolControl, ToolDiagnostic,
    ToolExecutionMode, ToolExecutionResult, ToolOutputLimits, ToolRegistration, UserInput,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::tasks::{TaskDefinition, TaskOwnership, define_task};
use pi_durable::types::{
    ConversationId, EntryRecord, JsonObject, JsonValue, SubmissionId, SubmissionState, SubmissionStatus, TaskId,
    TaskOptions, TaskOutcome, TaskQuery, TaskState,
};
use pi_js::vendor::typebox::builder as t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::chat_support::{ChatSetup, OpenChatEnv, all_entries, chat_setup, open_chat, text_of};
use support::harness_support::{Installed, add_hooks, add_task, add_tool, user};
use support::session_support::context;
use support::task_support::{aborted, deferred};
use tempfile::TempDir;

fn echo_parameters() -> JsonValue {
    t::object([("text", t::string().optional())]).to_value()
}

fn tool<F, Fut>(name: &str, execute: F) -> ToolRegistration
where
    F: Fn(JsonValue, pi_durable::harness::types::ToolExecutionApi, Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = pi_js::Result<ToolExecutionResult>> + Send + 'static,
{
    define_tool(ToolRegistration {
        name: name.to_owned(),
        description: format!("The {name} tool"),
        parameters: echo_parameters(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |args, api, ctx| Box::pin(execute(args, api, ctx))),
    })
}

fn calls(list: &[(&str, JsonObject, &str)]) -> AssistantMessage {
    let blocks: Vec<FauxContentBlock> = list
        .iter()
        .map(|(name, args, id)| {
            FauxContentBlock::ToolCall(faux_tool_call(
                name,
                args.clone(),
                Some(FauxToolCallOptions {
                    id: Some((*id).to_owned()),
                }),
            ))
        })
        .collect();
    faux_assistant_message(
        FauxAssistantMessageContent::Blocks(blocks),
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

fn obj(pairs: &[(&str, JsonValue)]) -> JsonObject {
    let mut map = JsonObject::new();
    for (k, v) in pairs {
        map.insert((*k).to_owned(), v.clone());
    }
    map
}

struct RunOutcome {
    harness: Harness,
    root: Conversation,
    entries: Vec<EntryRecord>,
    status: SubmissionStatus,
}

async fn run_chat<F, Fut>(
    setup: &ChatSetup,
    responses: Vec<FauxResponseStep>,
    prepare: Option<F>,
    env: OpenChatEnv,
) -> RunOutcome
where
    F: FnOnce(Harness, Conversation) -> Fut,
    Fut: Future<Output = ()>,
{
    setup.faux.set_responses(responses);
    let opened = open_chat(Arc::new(MemoryStorage::new()), setup, env)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
    if let Some(prep) = prepare {
        prep(opened.harness.clone(), opened.root.clone()).await;
    }
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
    let status = settled.record.status();
    let entries = all_entries(&opened.root, None).await.expect("all entries");
    RunOutcome {
        harness: opened.harness,
        root: opened.root,
        entries,
        status,
    }
}

async fn run_default(setup: &ChatSetup, responses: Vec<FauxResponseStep>) -> RunOutcome {
    let noop: Option<fn(Harness, Conversation) -> std::future::Ready<()>> = None;
    run_chat(setup, responses, noop, OpenChatEnv::None).await
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

fn result_text(message: &ToolResultMessage) -> String {
    message
        .content
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(t) => t.text.clone(),
            UserContentBlock::Image(_) => "[image]".to_owned(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

async fn poll_once<F: Future + Unpin>(fut: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|cx: &mut TaskContext<'_>| match Pin::new(&mut *fut).poll(cx) {
        Poll::Ready(val) => Poll::Ready(Some(val)),
        Poll::Pending => Poll::Ready(None),
    })
    .await
}

// ---------------------------------------------------------------------------
// describe("tool round")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runs_input_tool_call_tool_result_and_answer_and_settles_the_input() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("echo", |args, _, _| async move {
            let txt = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_owned();
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new(format!("echo {txt}")))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add echo");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("echo", obj(&[("text", json!("hi"))]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    assert_eq!(res.status, SubmissionStatus::Done);
    let kinds: Vec<&str> = res.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["pi.user", "pi.system", "pi.assistant", "pi.tool-result", "pi.assistant"]
    );
    let Some(Message::System(sys)) = res.entries[1].model.as_ref().and_then(|m| m.first()) else {
        panic!("expected system message");
    };
    let added = sys.tools_added.as_ref().expect("tools_added");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].name, "echo");
    assert_eq!(added[0].description, "The echo tool");
    assert!(added[0].parameters.is_object());

    let res_msgs = results(&res.entries);
    assert_eq!(res_msgs.len(), 1);
    let r = &res_msgs[0];
    assert_eq!(r.tool_call_id, "c1");
    assert_eq!(r.tool_name, "echo");
    assert!(!r.is_error);
    assert_eq!(result_text(r), "echo hi");
    let tr_entry = TOOL_RESULT_ENTRY
        .extract(Some(&res.entries[3]))
        .expect("extract tool result")
        .expect("is tool result");
    assert_eq!(tr_entry.data, ToolResultEntryData { diagnostics: vec![] });
    assert!(res.entries[3].by_task_id.is_some());
    assert_eq!(setup.faux.state().call_count(), 2);
    assert_eq!(
        res.harness
            .snapshot_conversation(&*LIVE_DOC, res.root.id(), context())
            .await
            .expect("live"),
        Some(LiveState::default())
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_calls_to_tools_the_request_did_not_offer_without_a_task() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("echo", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add echo");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("ghost", obj(&[]), "c1"), ("echo", obj(&[]), "c2")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    assert_eq!(res.status, SubmissionStatus::Done);
    let res_msgs = results(&res.entries);
    assert_eq!(res_msgs.len(), 2);
    let (ghost, echo) = (&res_msgs[0], &res_msgs[1]);
    assert_eq!(ghost.tool_call_id, "c1");
    assert!(ghost.is_error);
    assert_eq!(
        result_text(ghost),
        "<harness>\n[error] Tool ghost is not available\n</harness>"
    );
    assert_eq!(echo.tool_call_id, "c2");
    assert!(!echo.is_error);

    let ghost_entry = res
        .entries
        .iter()
        .find(|e| TOOL_RESULT_ENTRY.matches(e))
        .expect("ghost entry");
    let typed = TOOL_RESULT_ENTRY
        .extract(Some(ghost_entry))
        .expect("extract")
        .expect("some");
    assert_eq!(
        typed.data,
        ToolResultEntryData {
            diagnostics: vec![ToolDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: Some("tool_unavailable".to_owned()),
                message: "Tool ghost is not available".to_owned(),
            }]
        }
    );

    let root_id = res.root.id();
    let tasks = res
        .harness
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
        .expect("scan tasks");
    assert_eq!(tasks.items.iter().filter(|t| t.kind == "pi.tool").count(), 1);
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_a_call_to_a_tool_deactivated_after_preparation_with_tool_unavailable() {
    let setup = chat_setup(None);
    let seen = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let seen_cb = Arc::clone(&seen);
    add_tool(
        &setup.registry,
        tool("echo", move |_, _, _| {
            let seen_cb = Arc::clone(&seen_cb);
            async move {
                seen_cb.lock().unwrap().push("ran");
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add echo");

    let root_slot: Arc<Mutex<Option<Conversation>>> = Arc::new(Mutex::new(None));
    let root_for_step = Arc::clone(&root_slot);
    let root_for_prep = Arc::clone(&root_slot);

    let deactivate: FauxResponseStep = FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
        let root_for_step = Arc::clone(&root_for_step);
        Box::pin(async move {
            let conv = root_for_step.lock().unwrap().clone().expect("root set");
            conv.configure(
                AgentChange {
                    tools: FieldChange::Set(AgentToolsChange::replace(&[])),
                    ..Default::default()
                },
                context(),
            )
            .await
            .expect("configure empty tools");
            Ok(calls(&[("echo", obj(&[]), "c1")]))
        })
    }));

    let res = run_chat(
        &setup,
        vec![deactivate, FauxResponseStep::Message(done_msg())],
        Some(move |_harness: Harness, conversation: Conversation| {
            let root_for_prep = Arc::clone(&root_for_prep);
            async move {
                *root_for_prep.lock().unwrap() = Some(conversation);
            }
        }),
        OpenChatEnv::None,
    )
    .await;

    assert!(seen.lock().unwrap().is_empty());
    let res_msgs = results(&res.entries);
    assert_eq!(
        result_text(&res_msgs[0]),
        "<harness>\n[error] Tool echo is not available\n</harness>"
    );
    let last_sys = res
        .entries
        .iter()
        .filter(|e| e.kind == "pi.system")
        .last()
        .and_then(|e| e.model.as_ref()?.first())
        .and_then(|m| match m {
            Message::System(s) => Some(s.clone()),
            _ => None,
        })
        .expect("last system");
    let removed = last_sys.tools_removed.expect("tools_removed");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].name, "echo");
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn removes_unregistered_active_tools_from_the_offer_and_adds_them_back_after_re_registration() {
    let setup = chat_setup(None);
    let echo = tool("echo", |_, _, _| async {
        Ok(ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        })
    });
    let registration = add_tool(&setup.registry, echo.clone(), None).expect("add echo");
    let first = run_default(&setup, vec![FauxResponseStep::Message(done_msg())]).await;
    registration.dispose();

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("echo", obj(&[]), "c1")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let second = first
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("again".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit again")
        .wait(context())
        .await
        .expect("wait again");
    assert_eq!(second.record.status(), SubmissionStatus::Done);
    let entries = all_entries(&first.root, None).await.expect("entries");
    let removal = entries
        .iter()
        .filter(|e| e.kind == "pi.system")
        .last()
        .and_then(|e| match e.model.as_ref()?.first() {
            Some(Message::System(s)) => Some(s.clone()),
            _ => None,
        })
        .expect("removal system");
    let removed = removal.tools_removed.expect("tools_removed");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].name, "echo");
    assert!(results(&entries)[0].is_error);
    assert!(first.root.agent(context()).await.expect("agent").tools.is_empty());

    add_tool(&setup.registry, echo, None).expect("re-add echo");
    setup.faux.set_responses(vec![FauxResponseStep::Message(done_msg())]);
    first
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("back".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit back")
        .wait(context())
        .await
        .expect("wait back");
    let entries = all_entries(&first.root, None).await.expect("entries after back");
    let addition = entries
        .iter()
        .filter(|e| e.kind == "pi.system")
        .last()
        .and_then(|e| match e.model.as_ref()?.first() {
            Some(Message::System(s)) => Some(s.clone()),
            _ => None,
        })
        .expect("addition system");
    let added_names: Vec<String> = addition
        .tools_added
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(added_names, vec!["echo"]);
    first.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn produces_tool_unavailable_when_the_implementation_is_unregistered_before_its_task_runs() {
    let setup = chat_setup(None);
    let second_slot: Arc<Mutex<Option<Installed>>> = Arc::new(Mutex::new(None));
    let second_for_first = Arc::clone(&second_slot);

    let mut first_tool = tool("first", move |_, _, _| {
        let second_for_first = Arc::clone(&second_for_first);
        async move {
            if let Some(inst) = second_for_first.lock().unwrap().take() {
                inst.dispose();
            }
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }
    });
    first_tool.execution_mode = Some(ToolExecutionMode::Sequential);
    add_tool(&setup.registry, first_tool, None).expect("add first");

    let second_inst = add_tool(
        &setup.registry,
        tool("second", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("ran".to_owned()))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add second");
    *second_slot.lock().unwrap() = Some(second_inst);

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("first", obj(&[]), "c1"), ("second", obj(&[]), "c2")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    assert_eq!(res.status, SubmissionStatus::Done);
    let res_msgs = results(&res.entries);
    assert_eq!(res_msgs.len(), 2);
    assert_eq!(
        result_text(&res_msgs[1]),
        "<harness>\n[error] Tool second is not available\n</harness>"
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_the_execution_mode_when_a_round_starts_and_keeps_it_for_the_round() {
    let setup = chat_setup(None);
    let events = Arc::new(Mutex::new(Vec::<String>::new()));

    let make_slow = |name: &'static str,
                     events: Arc<Mutex<Vec<String>>>,
                     settings: Arc<support::chat_support::TestHarnessSettings>| {
        tool(name, move |_, _, _| {
            let events = Arc::clone(&events);
            let settings = Arc::clone(&settings);
            async move {
                events.lock().unwrap().push(format!("start {name}"));
                settings.update(|s| s.tool_execution = Some(ToolExecutionMode::Parallel));
                pi_js::time::sleep(Duration::from_millis(20)).await;
                events.lock().unwrap().push(format!("end {name}"));
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        })
    };

    add_tool(
        &setup.registry,
        make_slow("a", Arc::clone(&events), Arc::clone(&setup.settings)),
        None,
    )
    .expect("add a");
    add_tool(
        &setup.registry,
        make_slow("b", Arc::clone(&events), Arc::clone(&setup.settings)),
        None,
    )
    .expect("add b");

    let settings_for_req = Arc::clone(&setup.settings);
    let request = FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
        let settings_for_req = Arc::clone(&settings_for_req);
        Box::pin(async move {
            settings_for_req.update(|s| s.tool_execution = Some(ToolExecutionMode::Sequential));
            Ok(calls(&[("a", obj(&[]), "c1"), ("b", obj(&[]), "c2")]))
        })
    }));

    let res = run_default(&setup, vec![request, FauxResponseStep::Message(done_msg())]).await;
    assert_eq!(*events.lock().unwrap(), vec!["start a", "end a", "start b", "end b"]);
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_a_round_in_parallel_by_default_and_sequentially_when_configured_or_required_by_a_tool() {
    async fn trace(setup: &ChatSetup) -> Vec<String> {
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        for name in ["a", "b"] {
            if setup.registry.snapshot().extension(&format!("tool:{name}")).is_none() {
                let events_cb = Arc::clone(&events);
                add_tool(
                    &setup.registry,
                    tool(name, move |_, _, _| {
                        let events_cb = Arc::clone(&events_cb);
                        async move {
                            events_cb.lock().unwrap().push(format!("start {name}"));
                            pi_js::time::sleep(Duration::from_millis(20)).await;
                            events_cb.lock().unwrap().push(format!("end {name}"));
                            Ok(ToolExecutionResult {
                                content: Some(vec![]),
                                ..Default::default()
                            })
                        }
                    }),
                    None,
                )
                .expect("add slow");
            }
        }
        let res = run_default(
            setup,
            vec![
                FauxResponseStep::Message(calls(&[("a", obj(&[]), "c1"), ("b", obj(&[]), "c2")])),
                FauxResponseStep::Message(done_msg()),
            ],
        )
        .await;
        res.harness.close(context()).await.expect("close");
        events.lock().unwrap().clone()
    }

    let par = trace(&chat_setup(None)).await;
    assert_eq!(&par[..2], &["start a", "start b"]);

    let configured = chat_setup(None);
    configured
        .settings
        .update(|s| s.tool_execution = Some(ToolExecutionMode::Sequential));
    let sequential = trace(&configured).await;
    assert_eq!(sequential, vec!["start a", "end a", "start b", "end b"]);

    let per_tool = chat_setup(None);
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let ev_a = Arc::clone(&events);
    let mut tool_a = tool("a", move |_, _, _| {
        let ev_a = Arc::clone(&ev_a);
        async move {
            pi_js::time::sleep(Duration::from_millis(20)).await;
            ev_a.lock().unwrap().push("a");
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }
    });
    tool_a.execution_mode = Some(ToolExecutionMode::Sequential);
    add_tool(&per_tool.registry, tool_a, None).expect("add a");

    let ev_b = Arc::clone(&events);
    add_tool(
        &per_tool.registry,
        tool("b", move |_, _, _| {
            let ev_b = Arc::clone(&ev_b);
            async move {
                ev_b.lock().unwrap().push("b");
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add b");

    let result = run_default(
        &per_tool,
        vec![
            FauxResponseStep::Message(calls(&[("a", obj(&[]), "c1"), ("b", obj(&[]), "c2")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    let conv_id = result.root.id();
    let tasks = result
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
        .expect("scan tasks");
    let mut tool_tasks: Vec<_> = tasks.items.iter().filter(|t| t.kind == "pi.tool").collect();
    tool_tasks.sort_by_key(|t| t.id.as_i64());
    let generation = tasks
        .items
        .iter()
        .find(|t| t.kind == "pi.generation")
        .expect("generation task");
    let owners: Vec<_> = tool_tasks.iter().map(|t| t.owner).collect();
    assert_eq!(owners, vec![Some(generation.id), Some(generation.id)]);
    assert_eq!(*events.lock().unwrap(), vec!["a", "b"]);
    result.harness.close(context()).await.expect("close");
}

// ---------------------------------------------------------------------------
// describe("tool results")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uses_retained_output_and_the_last_details_when_the_result_omits_them_with_diagnostics_in_order() {
    let setup = chat_setup(None);
    let mut log_tool = tool("log", |_, api, _| async move {
        api.output("line 1\n").expect("output");
        api.output_bytes(b"line 2\nline 3\n").expect("output bytes");
        api.diagnostic(ToolDiagnostic {
            severity: DiagnosticSeverity::Info,
            message: "from api".to_owned(),
            code: None,
        })
        .expect("diag");
        api.details(&json!({ "step": 1 }), context()).await.expect("details 1");
        api.details(&json!({ "step": 2 }), context()).await.expect("details 2");
        Ok(ToolExecutionResult {
            diagnostics: Some(vec![ToolDiagnostic {
                severity: DiagnosticSeverity::Warn,
                message: "from result".to_owned(),
                code: None,
            }]),
            ..Default::default()
        })
    });
    log_tool.output_limits = Some(ToolOutputLimits {
        max_bytes: None,
        max_lines: Some(2),
        retain: None,
    });
    add_tool(&setup.registry, log_tool, None).expect("add log");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("log", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    let res_msgs = results(&res.entries);
    assert_eq!(res_msgs[0].details, Some(json!({ "step": 2 })));
    assert_eq!(
        result_text(&res_msgs[0]),
        "line 1\nline 2\n|<harness>\n[info] from api\n[warn] from result\n[warn] Output truncated to its beginning: 1 lines, 7 bytes dropped\n</harness>"
    );
    let entry = res
        .entries
        .iter()
        .find(|candidate| TOOL_RESULT_ENTRY.matches(candidate))
        .expect("entry");
    let typed = TOOL_RESULT_ENTRY.extract(Some(entry)).expect("ext").expect("some");
    let codes: Vec<Option<String>> = typed.data.diagnostics.into_iter().map(|d| d.code).collect();
    assert_eq!(codes, vec![None, None, Some("truncated".to_owned())]);
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn bounds_explicit_text_content_and_keeps_other_content() {
    let setup = chat_setup(None);
    let mut big = tool("big", |_, _, _| async move {
        Ok(ToolExecutionResult {
            content: Some(vec![
                UserContentBlock::Text(TextContent::new("a\nb\n".to_owned())),
                UserContentBlock::Image(ImageContent {
                    data: "AAAA".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
                UserContentBlock::Text(TextContent::new("c\nd\n".to_owned())),
            ]),
            ..Default::default()
        })
    });
    big.output_limits = Some(ToolOutputLimits {
        max_bytes: None,
        max_lines: Some(2),
        retain: Some(OutputRetainMode::Tail),
    });
    add_tool(&setup.registry, big, None).expect("add big");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("big", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(
        result_text(&results(&res.entries)[0]),
        "[image]|c\nd\n|<harness>\n[warn] Output truncated to its end: 2 lines, 4 bytes dropped\n</harness>"
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn turns_a_throw_into_a_tool_error_result_with_the_partial_output() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("fail", |_, api, _| async move {
            api.output("partial\n").expect("output");
            Err(pi_js::Error::msg("boom"))
        }),
        None,
    )
    .expect("add fail");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("fail", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(res.status, SubmissionStatus::Done);
    let r = &results(&res.entries)[0];
    assert!(r.is_error);
    assert_eq!(result_text(r), "partial\n|<harness>\n[error] boom\n</harness>");
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn validates_arguments_before_and_after_before_tool_and_applies_blocks_and_replacements() {
    let setup = chat_setup(None);
    let seen = Arc::new(Mutex::new(Vec::<JsonValue>::new()));
    let seen_cb = Arc::clone(&seen);
    add_tool(
        &setup.registry,
        tool("echo", move |args, _, _| {
            let seen_cb = Arc::clone(&seen_cb);
            async move {
                seen_cb.lock().unwrap().push(args);
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add echo");

    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            before_tool: Some(Arc::new(|call: ToolCall, _, _| {
                Box::pin(async move {
                    if call.id == "block" {
                        return Ok(Some(BeforeToolDecision::Block("not today".to_owned())));
                    }
                    if call.id == "throw" {
                        return Err(pi_js::Error::msg("hook failed"));
                    }
                    if call.id == "bad" {
                        return Ok(Some(BeforeToolDecision::Arguments(obj(&[(
                            "text",
                            json!({ "not": "a string" }),
                        )]))));
                    }
                    let text_val = call.arguments.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    Ok(Some(BeforeToolDecision::Arguments(obj(&[(
                        "text",
                        json!(format!("{text_val}!")),
                    )]))))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add before_tool hook");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[
                ("echo", obj(&[("text", json!(1))]), "coerced"),
                ("echo", obj(&[("text", json!({ "not": "a string" }))]), "invalid"),
                ("echo", obj(&[]), "block"),
                ("echo", obj(&[]), "throw"),
                ("echo", obj(&[("text", json!("x"))]), "bad"),
                ("echo", obj(&[("text", json!("x"))]), "ok"),
            ])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    let by_id: IndexMap<String, (bool, String)> = results(&res.entries)
        .into_iter()
        .map(|r| {
            let text = result_text(&r);
            (r.tool_call_id, (r.is_error, text))
        })
        .collect();

    assert_eq!(
        by_id.get("block"),
        Some(&(
            true,
            "<harness>\n[error] Tool call blocked: not today\n</harness>".to_owned()
        ))
    );
    assert_eq!(
        by_id.get("throw"),
        Some(&(
            true,
            "<harness>\n[error] Tool call blocked: hook failed\n</harness>".to_owned()
        ))
    );
    let bad = by_id.get("bad").expect("bad");
    assert!(bad.0);
    assert!(bad.1.contains("Validation failed"));
    assert_eq!(by_id.get("ok"), Some(&(false, String::new())));
    let invalid = by_id.get("invalid").expect("invalid");
    assert!(invalid.0);
    assert!(invalid.1.contains("Validation failed"));
    assert_eq!(by_id.get("coerced"), Some(&(false, String::new())));

    let seen_guard = seen.lock().unwrap();
    assert_eq!(seen_guard.len(), 2);
    assert!(seen_guard.contains(&json!({ "text": "1!" })));
    assert!(seen_guard.contains(&json!({ "text": "x!" })));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn repairs_arguments_with_prepare_arguments_before_validation_and_a_throwing_repair_is_invalid() {
    let setup = chat_setup(None);
    let seen = Arc::new(Mutex::new(Vec::<JsonValue>::new()));
    let seen_cb = Arc::clone(&seen);
    let mut echo = tool("echo", move |args, _, _| {
        let seen_cb = Arc::clone(&seen_cb);
        async move {
            seen_cb.lock().unwrap().push(args);
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }
    });
    echo.prepare_arguments = Some(Arc::new(|args| {
        let text = args.get("text");
        if text == Some(&json!("throw")) {
            return Err(pi_js::Error::msg("cannot repair"));
        }
        if let Some(n) = text.and_then(|v| v.as_f64()) {
            return Ok(json!({ "text": format!("#{}", pi_js::num::to_js_string(n)) }));
        }
        Ok(args.clone())
    }));
    add_tool(&setup.registry, echo, None).expect("add echo");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[
                ("echo", obj(&[("text", json!(7))]), "fixed"),
                ("echo", obj(&[("text", json!("throw"))]), "broken"),
            ])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    let by_id: IndexMap<String, String> = results(&res.entries)
        .into_iter()
        .map(|r| {
            let t = result_text(&r);
            (r.tool_call_id, t)
        })
        .collect();
    assert_eq!(by_id.get("fixed").map(String::as_str), Some(""));
    assert_eq!(
        by_id.get("broken").map(String::as_str),
        Some("<harness>\n[error] cannot repair\n</harness>")
    );
    assert_eq!(*seen.lock().unwrap(), vec![json!({ "text": "#7" })]);

    let Some(Message::Assistant(asst)) = res.entries[2].model.as_ref().and_then(|m| m.first()) else {
        panic!("expected assistant");
    };
    let call = asst
        .content
        .iter()
        .find_map(|block| match block {
            pi_ai::types::ContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(call.arguments.get("text"), Some(&json!(7)));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_the_first_before_tool_block_win_and_skips_later_handlers() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("echo", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add echo");

    let asked = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let asked_1 = Arc::clone(&asked);
    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            before_tool: Some(Arc::new(move |_, _, _| {
                let asked_1 = Arc::clone(&asked_1);
                Box::pin(async move {
                    asked_1.lock().unwrap().push("first");
                    Ok(Some(BeforeToolDecision::Block("first says no".to_owned())))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("hook 1");

    let asked_2 = Arc::clone(&asked);
    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            before_tool: Some(Arc::new(move |_, _, _| {
                let asked_2 = Arc::clone(&asked_2);
                Box::pin(async move {
                    asked_2.lock().unwrap().push("second");
                    Ok(Some(BeforeToolDecision::Block("second says no".to_owned())))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("hook 2");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("echo", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(*asked.lock().unwrap(), vec!["first"]);
    assert_eq!(
        result_text(&results(&res.entries)[0]),
        "<harness>\n[error] Tool call blocked: first says no\n</harness>"
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn chains_after_tool_replacements_and_observes_the_round_with_after_tools() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("echo", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("raw".to_owned()))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add echo");

    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            after_tool: Some(Arc::new(|_, mut result, _, _| {
                Box::pin(async move {
                    result.content = Some(vec![UserContentBlock::Text(TextContent::new("first".to_owned()))]);
                    Ok(Some(result))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("after 1");

    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            after_tool: Some(Arc::new(|_, mut result, _, _| {
                Box::pin(async move {
                    let rendered = result
                        .content
                        .as_ref()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(|item| match item {
                            UserContentBlock::Text(t) => t.text.clone(),
                            UserContentBlock::Image(_) => "[image]".to_owned(),
                        })
                        .collect::<Vec<_>>()
                        .join("|");
                    result.details = Some(json!({ "replaced": rendered }));
                    Ok(Some(result))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("after 2");

    let observed = Arc::new(Mutex::new(Vec::<(
        pi_durable::types::EntryId,
        Vec<pi_durable::types::EntryId>,
    )>::new()));
    let observed_cb = Arc::clone(&observed);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_tools: Some(Arc::new(move |assistant, entry_ids, _, _| {
                let observed_cb = Arc::clone(&observed_cb);
                Box::pin(async move {
                    observed_cb.lock().unwrap().push((assistant, entry_ids));
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("after_tools");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("echo", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;

    let r = &results(&res.entries)[0];
    assert_eq!(result_text(r), "first");
    assert_eq!(r.details, Some(json!({ "replaced": "first" })));
    let result_entry = res
        .entries
        .iter()
        .find(|e| TOOL_RESULT_ENTRY.matches(e))
        .expect("tool result entry");
    assert_eq!(
        *observed.lock().unwrap(),
        vec![(res.entries[2].id, vec![result_entry.id])]
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_the_hooks_of_the_selected_extensions_a_task_owned_child_copies_its_owners_selection() {
    let setup = chat_setup(None);
    let called_in = Arc::new(Mutex::new(Vec::<ConversationId>::new()));
    let called_in_cb = Arc::clone(&called_in);

    let echo_ext = define_extension(Extension {
        name: "echo".to_owned(),
        tools: vec![tool("echo", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        })],
        ..Default::default()
    });
    let audit_ext = define_extension(Extension {
        name: "audit".to_owned(),
        hooks: vec![hook(
            &*TOOL_TASK,
            ToolHooks {
                before_tool: Some(Arc::new(move |_, api, _| {
                    let called_in_cb = Arc::clone(&called_in_cb);
                    Box::pin(async move {
                        called_in_cb.lock().unwrap().push(api.conversation_id());
                        Ok(None)
                    })
                })),
                ..Default::default()
            },
        )],
        ..Default::default()
    });

    setup.registry.install(echo_ext).expect("install echo");
    setup.registry.install(audit_ext.clone()).expect("install audit");
    setup.settings.update(|s| s.extensions = Some(vec!["echo".to_owned()]));

    let first = run_default(&setup, vec![FauxResponseStep::Message(done_msg())]).await;
    first
        .root
        .configure(
            AgentChange {
                extensions: FieldChange::Set(AgentExtensionsChange::patch(Some(&[audit_ext]), None)),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure audit");

    let mut phases = IndexMap::new();
    phases.insert(
        "never".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) }))
            as pi_durable::tasks::TypedPhaseHandler<JsonValue, JsonObject, Option<JsonValue>, ()>,
    );
    let owner = define_task(TaskDefinition {
        name: "test.owner".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(JsonObject::new())),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let root_id = first.root.id();
    let child_id = first
        .harness
        .commit(
            move |tx| async move {
                let task_id = tx
                    .create_task(
                        &owner,
                        &json!({}),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(root_id),
                            background: false,
                        },
                    )
                    .await?;
                Ok(tx
                    .create_conversation(ConversationCreateOptions {
                        ownership: ConversationOwnership::Task {
                            task_id: task_id.erased(),
                        },
                        ..Default::default()
                    })
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("create child");
    let child = first
        .harness
        .conversation(child_id, context())
        .await
        .expect("get child")
        .expect("child exists");

    let other = first
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("create other");

    for conversation in [&first.root, &child, &other] {
        setup.faux.set_responses(vec![
            FauxResponseStep::Message(calls(&[("echo", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ]);
        conversation
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
    }

    assert_eq!(*called_in.lock().unwrap(), vec![first.root.id(), child.id()]);
    first.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn applies_add_tools_and_terminates_only_when_every_result_of_the_round_asks_to() {
    let setup = chat_setup(None);
    let stop = tool("stop", |_, _, _| async {
        Ok(ToolExecutionResult {
            content: Some(vec![]),
            control: Some(ToolControl {
                terminate: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        })
    });
    let grow = tool("grow", |_, _, _| async {
        Ok(ToolExecutionResult {
            content: Some(vec![]),
            control: Some(ToolControl {
                add_tools: Some(vec!["extra".to_owned(), "stop".to_owned()]),
                ..Default::default()
            }),
            ..Default::default()
        })
    });
    let extra = tool("extra", |_, _, _| async {
        Ok(ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        })
    });
    add_tool(&setup.registry, stop.clone(), None).expect("add stop");
    add_tool(&setup.registry, grow.clone(), None).expect("add grow");
    add_tool(&setup.registry, extra, None).expect("add extra");

    let stop_c = stop.clone();
    let grow_c = grow.clone();
    let first = run_chat(
        &setup,
        vec![FauxResponseStep::Message(calls(&[("stop", obj(&[]), "c1")]))],
        Some(move |_harness: Harness, root: Conversation| async move {
            root.configure(
                AgentChange {
                    tools: FieldChange::Set(AgentToolsChange::replace(&[stop_c, grow_c])),
                    ..Default::default()
                },
                context(),
            )
            .await
            .expect("configure");
        }),
        OpenChatEnv::None,
    )
    .await;

    assert_eq!(first.status, SubmissionStatus::Done);
    assert_eq!(first.entries.last().map(|e| e.kind.as_str()), Some("pi.tool-result"));
    let root_id = first.root.id();
    let settled = first
        .harness
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
        .expect("scan");
    assert!(
        settled
            .items
            .iter()
            .all(|t| matches!(t.state, TaskState::Terminal { .. }))
    );

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("stop", obj(&[]), "c1"), ("grow", obj(&[]), "c2")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let second = first
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("again".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit again")
        .wait(context())
        .await
        .expect("wait again");
    assert_eq!(second.record.status(), SubmissionStatus::Done);
    assert_eq!(
        all_entries(&first.root, None)
            .await
            .expect("entries")
            .last()
            .map(|e| e.kind.as_str()),
        Some("pi.assistant")
    );
    let agent_doc = first
        .harness
        .snapshot_conversation(&*AGENT_DOC, first.root.id(), context())
        .await
        .expect("agent doc")
        .expect("agent state");
    assert_eq!(
        agent_doc.tools,
        Some(AgentToolsFilter::List(vec![
            "stop".to_owned(),
            "grow".to_owned(),
            "extra".to_owned()
        ]))
    );
    first.harness.close(context()).await.expect("close");
}

// ---------------------------------------------------------------------------
// describe("generation hooks")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replaces_request_messages_observes_responses_and_continues_on_yield() {
    let setup = chat_setup(None);
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let req_for_step = Arc::clone(&requests);
    let record: FauxResponseStep = FauxResponseStep::Factory(Arc::new(move |request, _, _, _| {
        let req_for_step = Arc::clone(&req_for_step);
        Box::pin(async move {
            let mut guard = req_for_step.lock().unwrap();
            let line: Vec<String> = request
                .messages
                .iter()
                .map(|m| {
                    let role = match m {
                        Message::User(_) => "user",
                        Message::Assistant(_) => "assistant",
                        Message::ToolResult(_) => "toolResult",
                        Message::System(_) => "system",
                    };
                    format!("{role}:{}", text_of(m).unwrap_or(""))
                })
                .collect();
            guard.push(line);
            let n = guard.len();
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text(&format!("answer {n}")))),
                None,
            ))
        })
    }));

    let responses = Arc::new(Mutex::new(Vec::<String>::new()));
    let responses_cb = Arc::clone(&responses);
    let yields = Arc::new(AtomicUsize::new(0));
    let yields_cb = Arc::clone(&yields);

    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            before_request: Some(Arc::new(|mut messages, _, _| {
                Box::pin(async move {
                    messages.push(Message::User(user("injected")));
                    Ok(Some(messages))
                })
            })),
            after_response: Some(Arc::new(move |message, _, _| {
                let responses_cb = Arc::clone(&responses_cb);
                Box::pin(async move {
                    let msg = Message::Assistant(message);
                    responses_cb
                        .lock()
                        .unwrap()
                        .push(text_of(&msg).unwrap_or("").to_owned());
                    Ok(())
                })
            })),
            on_yield: Some(Arc::new(move |_, _, _| {
                let yields_cb = Arc::clone(&yields_cb);
                Box::pin(async move {
                    if yields_cb.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Some(UserInput::Blocks(vec![UserContentBlock::Text(TextContent::new(
                            "keep going".to_owned(),
                        ))])))
                    } else {
                        Ok(None)
                    }
                })
            })),
            after_tools: None,
        },
        None,
    )
    .expect("add hooks");

    let res = run_default(&setup, vec![record.clone(), record]).await;
    assert_eq!(res.status, SubmissionStatus::Done);
    let req_guard = requests.lock().unwrap();
    let tail = &req_guard[0][req_guard[0].len() - 2..];
    assert_eq!(tail, &["user:go".to_owned(), "user:injected".to_owned()]);
    assert_eq!(*responses.lock().unwrap(), vec!["answer 1", "answer 2"]);
    let kinds: Vec<&str> = res.entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["pi.user", "pi.assistant", "pi.user", "pi.assistant"]);
    assert!(
        !res.entries
            .iter()
            .any(|e| e.model.as_ref().and_then(|m| m.first()).and_then(text_of) == Some("injected"))
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_the_runs_input_open_across_an_on_yield_continuation_and_answers_it_with_the_final_answer() {
    let setup = chat_setup(None);
    let yields = Arc::new(AtomicUsize::new(0));
    let yields_cb = Arc::clone(&yields);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let yields_cb = Arc::clone(&yields_cb);
                Box::pin(async move {
                    if yields_cb.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Some(UserInput::Text("again".to_owned())))
                    } else {
                        Ok(None)
                    }
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add yield");

    let harness_slot: Arc<Mutex<Option<Harness>>> = Arc::new(Mutex::new(None));
    let input_slot: Arc<Mutex<Option<SubmissionId>>> = Arc::new(Mutex::new(None));
    let status_slot: Arc<Mutex<Option<SubmissionStatus>>> = Arc::new(Mutex::new(None));

    let h_prep = Arc::clone(&harness_slot);
    let h_step = Arc::clone(&harness_slot);
    let inp_step = Arc::clone(&input_slot);
    let st_step = Arc::clone(&status_slot);

    let second = FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
        let h_step = Arc::clone(&h_step);
        let inp_step = Arc::clone(&inp_step);
        let st_step = Arc::clone(&st_step);
        Box::pin(async move {
            let harness = h_step.lock().unwrap().clone().expect("harness");
            let live = harness
                .snapshot_conversation(&*LIVE_DOC, ConversationId::from_i64(1), context())
                .await
                .expect("live")
                .expect("live some");
            let input_id = live.run.expect("run").inputs[0];
            *inp_step.lock().unwrap() = Some(input_id);
            let sub = harness
                .submission(input_id, context())
                .await
                .expect("sub")
                .expect("sub some");
            let st = sub.status(context()).await.expect("status").status();
            *st_step.lock().unwrap() = Some(st);
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("second"))),
                None,
            ))
        })
    }));

    let res = run_chat(
        &setup,
        vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("first"))),
                None,
            )),
            second,
        ],
        Some(move |opened: Harness, _| async move {
            *h_prep.lock().unwrap() = Some(opened);
        }),
        OpenChatEnv::None,
    )
    .await;

    assert_eq!(*status_slot.lock().unwrap(), Some(SubmissionStatus::Placed));
    let answers: Vec<_> = res.entries.iter().filter(|e| e.kind == "pi.assistant").collect();
    let input_id = input_slot.lock().unwrap().expect("input id");
    let final_rec = res
        .harness
        .submission(input_id, context())
        .await
        .expect("sub")
        .expect("sub some")
        .status(context())
        .await
        .expect("final status");
    assert_eq!(
        final_rec.state,
        SubmissionState::Input(pi_durable::types::InputSubmissionState::Done {
            entry: final_rec.state.entry().expect("entry"),
            answer: answers[1].id,
        })
    );
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn observes_responses_that_arrive_by_polling_a_deferred_request() {
    let setup = chat_setup(Some(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: Some(1),
            poll_after_ms: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    }));
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_cb = Arc::clone(&observed);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_response: Some(Arc::new(move |message, _, _| {
                let observed_cb = Arc::clone(&observed_cb);
                Box::pin(async move {
                    let sr = match message.stop_reason {
                        StopReason::Stop => "stop",
                        StopReason::Length => "length",
                        StopReason::ToolUse => "toolUse",
                        StopReason::Error => "error",
                        StopReason::Aborted => "aborted",
                    };
                    let wrapped = Message::Assistant(message);
                    observed_cb
                        .lock()
                        .unwrap()
                        .push(format!("{sr}:{}", text_of(&wrapped).unwrap_or("")));
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add hook");
    setup.settings.update(|s| {
        s.stream.deferred = Some(StreamDeferredOption::Enabled(true));
    });

    let res = run_default(
        &setup,
        vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("late"))),
            None,
        ))],
    )
    .await;
    assert_eq!(res.status, SubmissionStatus::Done);
    assert_eq!(*observed.lock().unwrap(), vec!["stop:late"]);
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_the_first_on_yield_continuation_win_and_reports_throws_without_stopping_later_handlers() {
    let setup = chat_setup(None);
    let called = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let c1 = Arc::clone(&called);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_response: Some(Arc::new(move |_, _, _| {
                let c1 = Arc::clone(&c1);
                Box::pin(async move {
                    c1.lock().unwrap().push("throwing observer");
                    Err(pi_js::Error::msg("observer failed"))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("h1");

    let c2 = Arc::clone(&called);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_response: Some(Arc::new(move |_, _, _| {
                let c2 = Arc::clone(&c2);
                Box::pin(async move {
                    c2.lock().unwrap().push("next observer");
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("h2");

    let yields = Arc::new(AtomicUsize::new(0));
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let yields = Arc::clone(&yields);
                Box::pin(async move {
                    if yields.fetch_add(1, Ordering::SeqCst) == 0 {
                        Ok(Some(UserInput::Text("first".to_owned())))
                    } else {
                        Ok(None)
                    }
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("h3");

    let c4 = Arc::clone(&called);
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            on_yield: Some(Arc::new(move |_, _, _| {
                let c4 = Arc::clone(&c4);
                Box::pin(async move {
                    let mut g = c4.lock().unwrap();
                    g.push("second onYield");
                    let count = g.iter().filter(|n| **n == "second onYield").count();
                    if count == 1 {
                        Ok(Some(UserInput::Text("second".to_owned())))
                    } else {
                        Ok(None)
                    }
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("h4");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("a"))),
                None,
            )),
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("b"))),
                None,
            )),
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("c"))),
                None,
            )),
        ],
    )
    .await;

    let users: Vec<String> = res
        .entries
        .iter()
        .filter(|e| e.kind == "pi.user")
        .filter_map(|e| text_of(e.model.as_ref()?.first()?).map(str::to_owned))
        .collect();
    assert_eq!(users, vec!["go", "first", "second"]);
    let called_guard = called.lock().unwrap();
    assert_eq!(called_guard.iter().filter(|n| **n == "second onYield").count(), 2);
    assert_eq!(called_guard.iter().filter(|n| **n == "next observer").count(), 3);
    let report_msgs: Vec<String> = setup.reports.lock().unwrap().iter().map(|e| e.message()).collect();
    assert!(report_msgs.contains(&"observer failed".to_owned()));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_durable_hook_decisions_in_task_memos() {
    let setup = chat_setup(None);
    let asked = Arc::new(AtomicUsize::new(0));
    add_tool(
        &setup.registry,
        tool("echo", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add echo");

    let asked_cb = Arc::clone(&asked);
    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            before_tool: Some(Arc::new(move |_, api, _| {
                let asked_cb = Arc::clone(&asked_cb);
                Box::pin(async move {
                    asked_cb.fetch_add(1, Ordering::SeqCst);
                    let decision: String = api
                        .memo_set("approval:decision", &"approved".to_owned(), context())
                        .await?;
                    let second: String = api
                        .memo_set("approval:decision", &"denied".to_owned(), context())
                        .await?;
                    assert_eq!(second, decision);
                    Ok(None)
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add hook");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("echo", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    res.harness.close(context()).await.expect("close");
}

// ---------------------------------------------------------------------------
// describe("tool execution api")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn builds_the_environment_per_call_from_the_conversations_cwd_and_runs_commits_memos_and_child_tasks() {
    let setup = chat_setup(None);
    let mut child_phases = IndexMap::new();
    child_phases.insert(
        "run".to_owned(),
        Arc::new(
            |task: pi_durable::types::RunningTask<JsonValue, JsonObject, i64>,
             runtime: pi_durable::tasks::TaskRuntimeHandle<JsonValue, JsonObject, i64, ()>,
             call_ctx: Context| {
                Box::pin(async move {
                    let n = task.input.get("n").and_then(|v| v.as_i64()).expect("n");
                    runtime
                        .commit(
                            move |_tx, _cur| {
                                Box::pin(async move {
                                    Ok(Some(pi_durable::types::NextTaskState::Terminal {
                                        outcome: TaskOutcome::Completed { result: n * 2 },
                                    }))
                                })
                            },
                            call_ctx,
                        )
                        .await
                })
            },
        ) as pi_durable::tasks::TypedPhaseHandler<JsonValue, JsonObject, i64, ()>,
    );
    let child = define_task(TaskDefinition {
        name: "test.child".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(JsonObject::new())),
        phases: child_phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });
    add_task(&setup.registry, child.as_any().clone(), None).expect("add child");

    let seen = Arc::new(Mutex::new(Vec::<JsonValue>::new()));
    let seen_cb = Arc::clone(&seen);
    let child_c = child.clone();
    let probe = tool("probe", move |_, api, _| {
        let seen_cb = Arc::clone(&seen_cb);
        let child_c = child_c.clone();
        async move {
            let cwd_val = api
                .env()
                .map(|e| JsonValue::String(e.cwd().to_owned()))
                .unwrap_or(JsonValue::Null);
            let agent_tools: Vec<JsonValue> = api
                .agent(context())
                .await
                .expect("agent")
                .tools
                .into_iter()
                .map(|each| JsonValue::String(each.name))
                .collect();
            let has_probe = api.registry().extension("tool:probe").is_some();
            let conv_id = api.conversation_id();
            let call_id = api.call_id().to_owned();
            let entry = api
                .commit(
                    move |tx| async move {
                        pi_durable::harness::agent::configure(
                            &tx,
                            conv_id,
                            &AgentChange {
                                cwd: FieldChange::Set("/".to_owned()),
                                ..Default::default()
                            },
                        )
                        .await?;
                        tx.append_entry(
                            conv_id,
                            pi_durable::types::EntryDraft {
                                kind: "test.note".to_owned(),
                                model: None,
                                data: Some(JsonValue::String(call_id)),
                                head: None,
                                edits: None,
                            },
                        )
                        .await
                    },
                    context(),
                )
                .await
                .expect("commit");
            let by_task = entry.by_task_id == Some(api.task_id().erased());
            let m1: i64 = api.memo_set("m", &1i64, context()).await.expect("m1");
            let m2: i64 = api.memo_set("m", &2i64, context()).await.expect("m2");
            let id = api
                .create_task(
                    &child_c,
                    &json!({ "n": 21 }),
                    TaskOwnership::Conversation,
                    false,
                    context(),
                )
                .await
                .expect("create child");
            let done = api.wait_for_task(id, context()).await.expect("wait child");
            let mut g = seen_cb.lock().unwrap();
            g.push(cwd_val);
            g.push(JsonValue::Array(agent_tools));
            g.push(JsonValue::Bool(has_probe));
            g.push(JsonValue::Bool(by_task));
            g.push(json!(m1));
            g.push(json!(m2));
            g.push(serde_json::to_value(done.outcome).expect("ser outcome"));
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                ..Default::default()
            })
        }
    });
    add_tool(&setup.registry, probe, None).expect("add probe");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("probe", obj(&[]), "c1"), ("probe", obj(&[]), "c2")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    setup
        .settings
        .update(|s| s.tool_execution = Some(ToolExecutionMode::Sequential));

    let targets = Arc::new(Mutex::new(Vec::<(ConversationId, Option<String>)>::new()));
    let targets_cb = Arc::clone(&targets);
    let opened = open_chat(
        Arc::new(MemoryStorage::new()),
        &setup,
        OpenChatEnv::Factory(Arc::new(move |target, _| {
            let targets_cb = Arc::clone(&targets_cb);
            Box::pin(async move {
                targets_cb
                    .lock()
                    .unwrap()
                    .push((target.conversation_id, target.cwd.clone()));
                let env: Arc<dyn ExecutionEnv> =
                    Arc::new(NodeExecutionEnv::new(target.cwd.unwrap_or_else(|| "/tmp".to_owned())));
                Ok(Some(env))
            })
        })),
    )
    .await
    .expect("open chat");
    opened.harness.resume().expect("resume");
    opened
        .root
        .configure(
            AgentChange {
                cwd: FieldChange::Set("/tmp".to_owned()),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("configure /tmp");
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

    let outcome_json = json!({ "status": "completed", "result": 42 });
    let expected = vec![
        json!("/tmp"),
        json!(["probe"]),
        json!(true),
        json!(true),
        json!(1),
        json!(1),
        outcome_json.clone(),
        json!("/"),
        json!(["probe"]),
        json!(true),
        json!(true),
        json!(1),
        json!(1),
        outcome_json,
    ];
    assert_eq!(*seen.lock().unwrap(), expected);
    let tg = targets.lock().unwrap();
    assert!(tg.contains(&(opened.root.id(), Some("/tmp".to_owned()))));
    assert!(tg.contains(&(opened.root.id(), Some("/".to_owned()))));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_a_throwing_environment_with_a_tool_error_result_and_reports_it_once_while_preparing() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("probe", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("ran".to_owned()))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add probe");

    let noop: Option<fn(Harness, Conversation) -> std::future::Ready<()>> = None;
    let res = run_chat(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("probe", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
        noop,
        OpenChatEnv::Factory(Arc::new(|_, _| {
            Box::pin(async { Err(pi_js::Error::msg("no sandbox")) })
        })),
    )
    .await;

    let r = &results(&res.entries)[0];
    assert!(r.is_error);
    assert_eq!(result_text(r), "<harness>\n[error] no sandbox\n</harness>");
    assert_eq!(
        setup
            .reports
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.message() == "no sandbox")
            .count(),
        2
    );
    res.harness.close(context()).await.expect("close");
}

// ---------------------------------------------------------------------------
// describe("coding tools")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn answers_a_failing_command_with_its_retained_tail_and_diagnostics_in_order() {
    let dir = TempDir::new().expect("tempdir");
    let setup = chat_setup(None);
    add_tool(&setup.registry, pi_durable::tools::create_bash_tool(None), None).expect("add bash");
    let command = "i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done; exit 7";
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("bash", obj(&[("command", json!(command))]), "b")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.path().to_string_lossy().into_owned()));
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::One(env))
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
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

    let entries = all_entries(&opened.root, None).await.expect("entries");
    let entry = entries
        .iter()
        .find(|candidate| TOOL_RESULT_ENTRY.matches(candidate))
        .expect("entry");
    let res_msgs = results(std::slice::from_ref(entry));
    let r = &res_msgs[0];
    assert!(r.is_error);
    let txt = result_text(r);
    assert!(txt.starts_with("line-1001\n"), "got: {txt}");
    let typed = TOOL_RESULT_ENTRY.extract(Some(entry)).expect("ext").expect("some");
    let codes: Vec<Option<String>> = typed.data.diagnostics.into_iter().map(|d| d.code).collect();
    assert_eq!(
        codes,
        vec![
            Some("full_output".to_owned()),
            Some("tool_error".to_owned()),
            Some("truncated".to_owned()),
        ]
    );
    assert!(txt.contains("line-3000\n|<harness>\n[info] Full output: "));
    assert!(txt.contains("\n[error] Command exited with code 7\n[warn] Output truncated to its end: 1000 lines, "));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reads_edits_and_runs_a_command_in_one_run_then_answers() {
    let dir = TempDir::new().expect("tempdir");
    let notes_path = dir.path().join("notes.txt").to_string_lossy().into_owned();
    pi_js::fs::write_file_str(&notes_path, "hello world\n")
        .await
        .expect("write notes");
    let setup = chat_setup(None);
    setup
        .registry
        .install(define_extension(Extension {
            name: "coding".to_owned(),
            tools: vec![
                pi_durable::tools::create_read_tool(),
                pi_durable::tools::create_edit_tool(),
                pi_durable::tools::create_bash_tool(None),
            ],
            ..Default::default()
        }))
        .expect("install coding");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("read", obj(&[("path", json!("notes.txt"))]), "r")])),
        FauxResponseStep::Message(calls(&[(
            "edit",
            obj(&[
                ("path", json!("notes.txt")),
                ("edits", json!([{ "oldText": "world", "newText": "durable" }])),
            ]),
            "e",
        )])),
        FauxResponseStep::Message(calls(&[("bash", obj(&[("command", json!("cat notes.txt"))]), "b")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let env: Arc<dyn ExecutionEnv> = Arc::new(NodeExecutionEnv::new(dir.path().to_string_lossy().into_owned()));
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::One(env))
        .await
        .expect("open");
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
    let summary: Vec<(String, bool, String)> = results(&entries)
        .into_iter()
        .map(|r| {
            let txt = result_text(&r);
            (r.tool_name, r.is_error, txt)
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("read".to_owned(), false, "hello world\n".to_owned()),
            (
                "edit".to_owned(),
                false,
                "Successfully replaced 1 block(s) in notes.txt.".to_owned()
            ),
            ("bash".to_owned(), false, "hello durable\n".to_owned()),
        ]
    );
    assert_eq!(entries.last().map(|e| e.kind.as_str()), Some("pi.assistant"));
    let disk = pi_js::fs::read_file_to_string(&notes_path).await.expect("read notes");
    assert_eq!(disk, "hello durable\n");
    opened.harness.close(context()).await.expect("close");
}

// ---------------------------------------------------------------------------
// describe("tool progress and lifetime")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn applies_the_default_output_limits() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("lines", |_, api, _| async move {
            for index in 1..=2500 {
                api.output(&format!("{index}\n")).expect("output");
            }
            Ok(ToolExecutionResult::default())
        }),
        None,
    )
    .expect("add lines");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("lines", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    let txt = result_text(&results(&res.entries)[0]);
    assert!(txt.starts_with("1\n2\n"));
    assert!(txt.ends_with(
        "\n2000\n|<harness>\n[warn] Output truncated to its beginning: 500 lines, 2500 bytes dropped\n</harness>"
    ));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn sanitizes_running_output_but_keeps_explicit_result_content_as_the_tool_returned_it() {
    let setup = chat_setup(None);
    let slot_output = Arc::new(Mutex::new(None::<String>));
    let slot_cb = Arc::clone(&slot_output);

    add_tool(
        &setup.registry,
        tool("noisy", move |_, api, _| {
            let slot_cb = Arc::clone(&slot_cb);
            async move {
                api.output("a\x07b\r\n").expect("output");
                api.details(&json!({ "ready": true }), context())
                    .await
                    .expect("details");
                let snap = api
                    .read()
                    .snapshot_conversation(&*LIVE_DOC, api.conversation_id(), context())
                    .await
                    .expect("snap");
                *slot_cb.lock().unwrap() = snap
                    .and_then(|s| s.tools)
                    .and_then(|mut v| v.pop())
                    .and_then(|slot| slot.output);
                Ok(ToolExecutionResult::default())
            }
        }),
        None,
    )
    .expect("add noisy");
    add_tool(
        &setup.registry,
        tool("explicit", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("c\x1bd".to_owned()))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add explicit");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("noisy", obj(&[]), "c1"), ("explicit", obj(&[]), "c2")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(*slot_output.lock().unwrap(), Some("ab\n".to_owned()));
    let by_id: IndexMap<String, String> = results(&res.entries)
        .into_iter()
        .map(|r| {
            let txt = result_text(&r);
            (r.tool_call_id, txt)
        })
        .collect();
    assert_eq!(by_id.get("c1").map(String::as_str), Some("ab\n"));
    assert_eq!(by_id.get("c2").map(String::as_str), Some("c\x1bd"));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn drops_control_keys_set_to_undefined_instead_of_faulting() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("grow", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                control: Some(ToolControl {
                    add_tools: Some(vec!["extra".to_owned()]),
                    terminate: None,
                    handoff: None,
                }),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add grow");
    let extra = tool("extra", |_, _, _| async {
        Ok(ToolExecutionResult {
            content: Some(vec![]),
            ..Default::default()
        })
    });
    add_tool(&setup.registry, extra.clone(), None).expect("add extra");

    let extra_c = extra.clone();
    let res = run_chat(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("grow", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
        Some(move |_harness: Harness, conversation: Conversation| async move {
            conversation
                .configure(
                    AgentChange {
                        tools: FieldChange::Set(AgentToolsChange::remove(&[extra_c])),
                        ..Default::default()
                    },
                    context(),
                )
                .await
                .expect("configure remove");
        }),
        OpenChatEnv::None,
    )
    .await;
    assert_eq!(res.status, SubmissionStatus::Done);
    let agent = res
        .harness
        .snapshot_conversation(&*AGENT_DOC, res.root.id(), context())
        .await
        .expect("agent")
        .expect("some");
    assert_eq!(agent.tools, Some(AgentToolsFilter::Remove { remove: vec![] }));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn uses_explicit_null_details_instead_of_the_last_reported_value() {
    let setup = chat_setup(None);
    add_tool(
        &setup.registry,
        tool("null", |_, api, _| async move {
            api.details(&json!({ "old": 1 }), context()).await.expect("old details");
            Ok(ToolExecutionResult {
                content: Some(vec![]),
                details: Some(JsonValue::Null),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("add null");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("null", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    assert_eq!(results(&res.entries)[0].details, Some(JsonValue::Null));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn settles_details_promises_with_coalesced_progress_commits_and_the_terminal_commit() {
    let setup = chat_setup(None);
    let settled = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let bg_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<()>>));
    let settled_cb = Arc::clone(&settled);
    let bg_cb = Arc::clone(&bg_handle);

    add_tool(
        &setup.registry,
        tool("details", move |_, api, _| {
            let settled_cb = Arc::clone(&settled_cb);
            let bg_cb = Arc::clone(&bg_cb);
            async move {
                let s1 = Arc::clone(&settled_cb);
                let api1 = api.clone();
                let mut f1 = Box::pin(async move {
                    api1.details(&json!({ "n": 1 }), context()).await.expect("d1");
                    s1.lock().unwrap().push("first");
                });
                let s2 = Arc::clone(&settled_cb);
                let api2 = api.clone();
                let mut f2 = Box::pin(async move {
                    api2.details(&json!({ "n": 2 }), context()).await.expect("d2");
                    s2.lock().unwrap().push("second");
                });

                let mut f1_done = poll_once(&mut f1).await.is_some();
                let mut f2_done = poll_once(&mut f2).await.is_some();
                if !f1_done {
                    let mut f1_opt = Some(f1);
                    let mut f2_opt = if f2_done { None } else { Some(f2) };
                    let remaining_f2 = std::future::poll_fn(|cx: &mut TaskContext<'_>| {
                        if let Some(ref mut fut1) = f1_opt
                            && fut1.as_mut().poll(cx).is_ready()
                        {
                            f1_opt = None;
                        }
                        if let Some(ref mut fut2) = f2_opt
                            && fut2.as_mut().poll(cx).is_ready()
                        {
                            f2_opt = None;
                        }
                        if f1_opt.is_none() {
                            Poll::Ready(f2_opt.take())
                        } else {
                            Poll::Pending
                        }
                    })
                    .await;
                    f1_done = true;
                    f2_done = remaining_f2.is_none();
                    f2_opt = remaining_f2;
                    let s3 = Arc::clone(&settled_cb);
                    let api3 = api.clone();
                    let mut f3 = Box::pin(async move {
                        api3.details(&json!({ "n": 3 }), context()).await.expect("d3");
                        s3.lock().unwrap().push("third");
                    });
                    let f3_done = poll_once(&mut f3).await.is_some();
                    if f2_opt.is_some() || !f3_done {
                        let mut f3_opt = if f3_done { None } else { Some(f3) };
                        let handle = tokio::spawn(async move {
                            std::future::poll_fn(|cx: &mut TaskContext<'_>| {
                                if let Some(ref mut fut2) = f2_opt
                                    && fut2.as_mut().poll(cx).is_ready()
                                {
                                    f2_opt = None;
                                }
                                if let Some(ref mut fut3) = f3_opt
                                    && fut3.as_mut().poll(cx).is_ready()
                                {
                                    f3_opt = None;
                                }
                                if f2_opt.is_none() && f3_opt.is_none() {
                                    Poll::Ready(())
                                } else {
                                    Poll::Pending
                                }
                            })
                            .await
                        });
                        *bg_cb.lock().unwrap() = Some(handle);
                    }
                    let _ = (f1_done, f2_done);
                    return Ok(ToolExecutionResult {
                        content: Some(vec![]),
                        ..Default::default()
                    });
                }

                let mut f2_opt = if f2_done { None } else { Some(f2) };
                let s3 = Arc::clone(&settled_cb);
                let api3 = api.clone();
                let mut f3 = Box::pin(async move {
                    api3.details(&json!({ "n": 3 }), context()).await.expect("d3");
                    s3.lock().unwrap().push("third");
                });
                let f3_done = poll_once(&mut f3).await.is_some();
                if f2_opt.is_some() || !f3_done {
                    let mut f3_opt = if f3_done { None } else { Some(f3) };
                    let handle = tokio::spawn(async move {
                        std::future::poll_fn(|cx: &mut TaskContext<'_>| {
                            if let Some(ref mut fut2) = f2_opt
                                && fut2.as_mut().poll(cx).is_ready()
                            {
                                f2_opt = None;
                            }
                            if let Some(ref mut fut3) = f3_opt
                                && fut3.as_mut().poll(cx).is_ready()
                            {
                                f3_opt = None;
                            }
                            if f2_opt.is_none() && f3_opt.is_none() {
                                Poll::Ready(())
                            } else {
                                Poll::Pending
                            }
                        })
                        .await
                    });
                    *bg_cb.lock().unwrap() = Some(handle);
                }
                let _ = (f1_done, f2_done);
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add details");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("details", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    if let Some(h) = bg_handle.lock().unwrap().take() {
        h.await.expect("join background details");
    }
    assert_eq!(*settled.lock().unwrap(), vec!["first", "second", "third"]);
    assert_eq!(results(&res.entries)[0].details, Some(json!({ "n": 3 })));
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn finishes_a_call_under_the_implementation_it_resolved_when_the_tool_is_replaced_mid_call() {
    let setup = chat_setup(None);
    let started = deferred();
    let finished = deferred();
    let started_cb = started.clone();
    let finished_cb = finished.clone();

    let v1 = tool("work", move |_, _, _| {
        let started_cb = started_cb.clone();
        let finished_cb = finished_cb.clone();
        async move {
            started_cb.resolve(());
            finished_cb.wait().await.expect("finished");
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("v1".to_owned()))]),
                ..Default::default()
            })
        }
    });
    add_tool(&setup.registry, v1, None).expect("add v1");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("work", obj(&[]), "c1")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
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
    started.wait().await.expect("started");
    add_tool(
        &setup.registry,
        tool("work", |_, _, _| async {
            Ok(ToolExecutionResult {
                content: Some(vec![UserContentBlock::Text(TextContent::new("v2".to_owned()))]),
                ..Default::default()
            })
        }),
        None,
    )
    .expect("replace work");
    finished.resolve(());
    submission.wait(context()).await.expect("wait");
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(result_text(&results(&entries)[0]), "v1");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn uses_a_section_and_hook_extension_reloaded_mid_run_from_the_runs_next_request() {
    let setup = chat_setup(None);
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));

    let prompt = |version: &'static str, requests: Arc<Mutex<Vec<String>>>| {
        define_extension(Extension {
            name: "prompt".to_owned(),
            sections: vec![section(
                "mode",
                Arc::new(move |_, _| Box::pin(async move { Ok(Some(version.to_owned())) })),
            )],
            hooks: vec![hook(
                &*GENERATION_TASK,
                GenerationHooks {
                    before_request: Some(Arc::new(move |_, _, _| {
                        let requests = Arc::clone(&requests);
                        Box::pin(async move {
                            requests.lock().unwrap().push(version.to_owned());
                            Ok(None)
                        })
                    })),
                    ..Default::default()
                },
            )],
            ..Default::default()
        })
    };

    setup
        .registry
        .install(prompt("v1", Arc::clone(&requests)))
        .expect("install v1");
    let running = deferred();
    let reloaded = deferred();
    let running_cb = running.clone();
    let reloaded_cb = reloaded.clone();
    add_tool(
        &setup.registry,
        tool("work", move |_, _, _| {
            let running_cb = running_cb.clone();
            let reloaded_cb = reloaded_cb.clone();
            async move {
                running_cb.resolve(());
                reloaded_cb.wait().await.expect("reloaded");
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add work");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("work", obj(&[]), "c1")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
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
    running.wait().await.expect("running");
    setup
        .registry
        .install(prompt("v2", Arc::clone(&requests)))
        .expect("install v2");
    reloaded.resolve(());
    submission.wait(context()).await.expect("wait");

    let sections: Vec<IndexMap<String, Option<String>>> = all_entries(&opened.root, None)
        .await
        .expect("entries")
        .into_iter()
        .filter_map(|e| match e.model.as_ref()?.first() {
            Some(Message::System(sys)) => sys.sections.clone(),
            _ => None,
        })
        .collect();
    let mut s1 = IndexMap::new();
    s1.insert("mode".to_owned(), Some("<mode>\nv1\n</mode>".to_owned()));
    let mut s2 = IndexMap::new();
    s2.insert("mode".to_owned(), Some("<mode>\nv2\n</mode>".to_owned()));
    assert_eq!(sections, vec![s1, s2]);
    assert_eq!(*requests.lock().unwrap(), vec!["v1", "v2"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_invocation_bound_waits_and_stops_watches_when_the_tools_invocation_ends() {
    let setup = chat_setup(None);
    let mut never_phases = IndexMap::new();
    never_phases.insert(
        "never".to_owned(),
        Arc::new(|_, _, _| Box::pin(async { Ok(()) }))
            as pi_durable::tasks::TypedPhaseHandler<JsonValue, JsonObject, Option<JsonValue>, ()>,
    );
    let never = define_task(TaskDefinition {
        name: "test.never".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(JsonObject::new())),
        phases: never_phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    });

    let wait_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<pi_js::Result<()>>>));
    let watch_closed_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<pi_durable::types::WatchEnd>>));
    let wait_slot = Arc::clone(&wait_handle);
    let watch_slot = Arc::clone(&watch_closed_handle);

    add_tool(
        &setup.registry,
        tool("detach", move |_, api, _| {
            let never = never.clone();
            let wait_slot = Arc::clone(&wait_slot);
            let watch_slot = Arc::clone(&watch_slot);
            async move {
                let child = api
                    .create_task(&never, &json!({}), TaskOwnership::Conversation, false, context())
                    .await
                    .expect("create child");
                let api_w = api.clone();
                let mut wait_fut = Box::pin(async move { api_w.wait_for_task(child, context()).await.map(|_| ()) });
                assert!(poll_once(&mut wait_fut).await.is_none(), "child should stay pending");
                *wait_slot.lock().unwrap() = Some(tokio::spawn(wait_fut));

                let watch = api
                    .watch_conversation_doc(&*LIVE_DOC, api.conversation_id(), context())
                    .await
                    .expect("watch")
                    .expect("watch some");
                *watch_slot.lock().unwrap() = Some(tokio::spawn(async move { watch.closed().await }));
                Ok(ToolExecutionResult {
                    content: Some(vec![]),
                    ..Default::default()
                })
            }
        }),
        None,
    )
    .expect("add detach");

    let res = run_default(
        &setup,
        vec![
            FauxResponseStep::Message(calls(&[("detach", obj(&[]), "c1")])),
            FauxResponseStep::Message(done_msg()),
        ],
    )
    .await;
    let wait_res = wait_handle
        .lock()
        .unwrap()
        .take()
        .expect("wait handle")
        .await
        .expect("join");
    let err = wait_res.expect_err("wait should reject");
    assert!(
        err.message().contains("invocation has ended"),
        "unexpected error: {}",
        err.message()
    );
    watch_closed_handle
        .lock()
        .unwrap()
        .take()
        .expect("watch closed")
        .await
        .expect("watch closed join");
    res.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_details_still_waiting_when_the_call_is_aborted_during_after_tool() {
    let setup = chat_setup(None);
    let pending_details = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<pi_js::Result<()>>>));
    let pending_slot = Arc::clone(&pending_details);
    let in_after_tool = deferred();
    let in_after_cb = in_after_tool.clone();

    add_tool(
        &setup.registry,
        tool("slow", move |_, api, _| {
            let pending_slot = Arc::clone(&pending_slot);
            async move {
                api.output("first\n").expect("output");
                let mut fut = Box::pin(async move { api.details(&json!({ "step": 1 }), context()).await });
                let early = poll_once(&mut fut).await;
                if let Some(res) = early {
                    *pending_slot.lock().unwrap() = Some(tokio::spawn(async move { res }));
                } else {
                    *pending_slot.lock().unwrap() = Some(tokio::spawn(fut));
                }
                Ok(ToolExecutionResult::default())
            }
        }),
        None,
    )
    .expect("add slow");

    add_hooks(
        &setup.registry,
        &*TOOL_TASK,
        ToolHooks {
            after_tool: Some(Arc::new(move |_, _, _, call_ctx| {
                let in_after_cb = in_after_cb.clone();
                Box::pin(async move {
                    in_after_cb.resolve(());
                    let signal = call_ctx.abort_signal().expect("signal");
                    aborted::<()>(&signal).await?;
                    Ok(None)
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add after_tool");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("slow", obj(&[]), "c1")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
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
    in_after_tool.wait().await.expect("in after tool");
    let task_id = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("some")
        .tools
        .expect("tools")[0]
        .task_id
        .expect("task_id");
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    let det_res = pending_details
        .lock()
        .unwrap()
        .take()
        .expect("pending details")
        .await
        .expect("join");
    assert!(det_res.is_err());
    submission.wait(context()).await.expect("wait");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_an_aborted_tool_with_only_its_durable_output_discarding_buffered_output() {
    let setup = chat_setup(None);
    let buffered = deferred();
    let buffered_cb = buffered.clone();
    let aborted_seen = Arc::new(AtomicBool::new(false));
    let aborted_seen_cb = Arc::clone(&aborted_seen);

    add_tool(
        &setup.registry,
        tool("slow", move |_, api, call_ctx| {
            let buffered_cb = buffered_cb.clone();
            let aborted_seen_cb = Arc::clone(&aborted_seen_cb);
            async move {
                api.output("durable\n").expect("durable");
                pi_js::time::sleep(Duration::from_millis(20)).await;
                api.output("buffered\n").expect("buffered");
                buffered_cb.resolve(());
                let signal = call_ctx.abort_signal().expect("signal");
                let err = aborted::<()>(&signal).await.unwrap_err();
                aborted_seen_cb.store(true, Ordering::SeqCst);
                Err(err)
            }
        }),
        None,
    )
    .expect("add slow");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(calls(&[("slow", obj(&[]), "c1")])),
        FauxResponseStep::Message(done_msg()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
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
    buffered.wait().await.expect("buffered");
    let task_id = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("some")
        .tools
        .expect("tools")[0]
        .task_id
        .expect("task_id");
    opened.harness.abort_task(task_id, context()).await.expect("abort");
    submission.wait(context()).await.expect("wait");
    assert!(aborted_seen.load(Ordering::SeqCst));
    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(
        result_text(&results(&entries)[0]),
        "durable\n|<harness>\n[error] Tool slow was aborted\n</harness>"
    );
    opened.harness.close(context()).await.expect("close");
}

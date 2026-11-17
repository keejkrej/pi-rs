//! Port of packages/coding-agent/test/suite/agent-session-bash-persistence.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxContentBlock, FauxAssistantMessageOptions, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock};
use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, CustomMessageDelivery, ExecuteBashOptions, SendCustomMessageDraft,
};
use pi_coding_agent::core::bash_executor::BashResult;
use pi_coding_agent::core::extensions::types::CustomMessageContent;
use pi_coding_agent::core::tools::bash::{BashExecOptions, BashExecResult, BashOperations};
use pi_js::abort::AbortSignal;
use pi_js::BoxFuture;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::suite::harness::{Harness, HarnessOptions, create_harness};

struct ControlledInvocation {
    signal: Option<AbortSignal>,
    tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl ControlledInvocation {
    fn finish(&self) {
        if let Some(tx) = self.tx.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = tx.send(());
        }
    }
}

struct ControlledBashOps {
    invocations: Mutex<Vec<Arc<ControlledInvocation>>>,
}

impl ControlledBashOps {
    fn new() -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn list(&self) -> Vec<Arc<ControlledInvocation>> {
        self.invocations.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl BashOperations for ControlledBashOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let inv = Arc::new(ControlledInvocation {
            signal: options.signal,
            tx: Mutex::new(Some(tx)),
        });
        self.invocations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(inv);
        Box::pin(async move {
            let _ = rx.await;
            Ok(BashExecResult { exit_code: Some(0) })
        })
    }
}

struct AbortOnSignalOps;

impl BashOperations for AbortOnSignalOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let tx_arc = Arc::new(Mutex::new(Some(tx)));
            let _guard = options.signal.as_ref().map(|sig| {
                let tx_inner = Arc::clone(&tx_arc);
                sig.add_listener(move || {
                    if let Some(sender) = tx_inner.lock().unwrap_or_else(PoisonError::into_inner).take() {
                        let _ = sender.send(());
                    }
                })
            });
            let _ = rx.await;
            Err(pi_js::Error::msg("aborted"))
        })
    }
}

struct FnBashOps<F>(F);

impl<F> BashOperations for FnBashOps<F>
where
    F: Fn(BashExecOptions) -> BoxFuture<pi_js::Result<BashExecResult>> + Send + Sync,
{
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions,
    ) -> BoxFuture<pi_js::Result<BashExecResult>> {
        (self.0)(options)
    }
}

fn entry_types(harness: &Harness) -> Vec<&'static str> {
    harness
        .session_manager
        .get_entries()
        .into_iter()
        .map(|e| match e {
            pi_coding_agent::core::session_manager::SessionEntry::Message(_) => "message",
            pi_coding_agent::core::session_manager::SessionEntry::CustomMessage(_) => "custom_message",
            pi_coding_agent::core::session_manager::SessionEntry::ModelChange(_) => "model_change",
            pi_coding_agent::core::session_manager::SessionEntry::ThinkingLevelChange(_) => "thinking_level_change",
            pi_coding_agent::core::session_manager::SessionEntry::Compaction(_) => "compaction",
            pi_coding_agent::core::session_manager::SessionEntry::BranchSummary(_) => "branch_summary",
            pi_coding_agent::core::session_manager::SessionEntry::Custom(_) => "custom",
            pi_coding_agent::core::session_manager::SessionEntry::SessionInfo(_) => "session_info",
            pi_coding_agent::core::session_manager::SessionEntry::Label(_) => "label",
            pi_coding_agent::core::session_manager::SessionEntry::Usage(_) => "usage",
            pi_coding_agent::core::session_manager::SessionEntry::ContextEdit(_) => "context_edit",
        })
        .collect()
}

#[tokio::test]
async fn records_bash_results_immediately_while_idle() {
    let harness = create_harness(HarnessOptions::default()).await;

    harness.session.record_bash_result(
        "echo hi",
        &BashResult {
            output: "hi".to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
        },
        None,
    );

    assert!(!harness.session.has_pending_bash_messages());
    let msgs = harness.session.messages();
    assert_eq!(msgs.last().and_then(|m| m.role()), Some("bashExecution"));
    assert!(entry_types(&harness).contains(&"message"));

    harness.cleanup();
}

#[tokio::test]
async fn defers_bash_results_while_streaming_and_flushes_them_before_the_next_prompt() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for release".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rx_slot = Arc::clone(&release_rx);
            Box::pin(async move {
                if let Some(rx) = rx_slot.lock().await.take() {
                    let _ = rx.await;
                }
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                serde_json::Map::new(),
                None,
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("after flush".to_string()),
            None,
        )),
    ]);

    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let start_slot = Arc::new(Mutex::new(Some(start_tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::ToolExecutionStart { .. })
            && let Some(tx) = start_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = tx.send(());
        }
    });

    let sess_bg = harness.session.clone();
    let first_task = tokio::spawn(async move { sess_bg.prompt("start", None).await });
    let _ = start_rx.await;

    harness.session.record_bash_result(
        "echo hi",
        &BashResult {
            output: "hi".to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
        },
        None,
    );

    assert!(harness.session.has_pending_bash_messages());
    assert!(!harness.session.messages().iter().any(|m| m.role() == Some("bashExecution")));

    let _ = release_tx.send(());
    first_task.await.unwrap().unwrap();

    assert!(!harness.session.has_pending_bash_messages());
    assert!(harness.session.messages().iter().any(|m| m.role() == Some("bashExecution")));

    harness.session.prompt("next turn", None).await.unwrap();
    assert!(!harness.session.has_pending_bash_messages());
    assert!(harness.session.messages().iter().any(|m| m.role() == Some("bashExecution")));
    assert!(entry_types(&harness).iter().filter(|t| **t == "message").count() > 0);

    harness.cleanup();
}

#[tokio::test]
async fn executes_bash_commands_and_records_the_result() {
    let harness = create_harness(HarnessOptions::default()).await;
    let res = harness.session.execute_bash("printf 'hello'", None, None).await.unwrap();
    assert!(res.output.contains("hello"));
    assert_eq!(harness.session.messages().last().and_then(|m| m.role()), Some("bashExecution"));
    harness.cleanup();
}

#[tokio::test]
async fn cancels_running_bash_commands_with_abort_bash() {
    let harness = create_harness(HarnessOptions::default()).await;
    let ops: Arc<dyn BashOperations> = Arc::new(AbortOnSignalOps);
    let sess = harness.session.clone();
    let bash_task = tokio::spawn(async move {
        sess.execute_bash(
            "sleep",
            None,
            Some(ExecuteBashOptions {
                operations: Some(ops),
                ..Default::default()
            }),
        )
        .await
    });
    pi_js::time::sleep(10).await;
    assert!(harness.session.is_bash_running());
    harness.session.abort_bash();

    let result = bash_task.await.unwrap().unwrap();
    assert!(result.cancelled);
    assert!(!harness.session.is_bash_running());
    harness.cleanup();
}

#[tokio::test]
async fn keeps_newer_bash_execution_tracked_when_an_older_execution_finishes() {
    let harness = create_harness(HarnessOptions::default()).await;
    let ops = Arc::new(ControlledBashOps::new());

    let s1 = harness.session.clone();
    let o1: Arc<dyn BashOperations> = ops.clone();
    let first_task = tokio::spawn(async move {
        s1.execute_bash(
            "first",
            None,
            Some(ExecuteBashOptions {
                operations: Some(o1),
                ..Default::default()
            }),
        )
        .await
    });
    let s2 = harness.session.clone();
    let o2: Arc<dyn BashOperations> = ops.clone();
    let second_task = tokio::spawn(async move {
        s2.execute_bash(
            "second",
            None,
            Some(ExecuteBashOptions {
                operations: Some(o2),
                ..Default::default()
            }),
        )
        .await
    });
    while ops.list().len() < 2 {
        pi_js::time::sleep(5).await;
    }

    let invs = ops.list();
    invs[0].finish();
    let first_result = first_task.await.unwrap().unwrap();
    let running_after_first = harness.session.is_bash_running();

    harness.session.abort_bash();
    let second_aborted = invs[1].signal.as_ref().is_some_and(|s| s.aborted());
    invs[1].finish();
    let second_result = second_task.await.unwrap().unwrap();

    assert!(!first_result.cancelled);
    assert!(running_after_first);
    assert!(second_aborted);
    assert!(second_result.cancelled);
    assert!(!harness.session.is_bash_running());
    harness.cleanup();
}

#[tokio::test]
async fn aborts_all_active_bash_executions() {
    let harness = create_harness(HarnessOptions::default()).await;
    let ops = Arc::new(ControlledBashOps::new());

    let s1 = harness.session.clone();
    let o1: Arc<dyn BashOperations> = ops.clone();
    let first_task = tokio::spawn(async move {
        s1.execute_bash(
            "first",
            None,
            Some(ExecuteBashOptions {
                operations: Some(o1),
                ..Default::default()
            }),
        )
        .await
    });
    let s2 = harness.session.clone();
    let o2: Arc<dyn BashOperations> = ops.clone();
    let second_task = tokio::spawn(async move {
        s2.execute_bash(
            "second",
            None,
            Some(ExecuteBashOptions {
                operations: Some(o2),
                ..Default::default()
            }),
        )
        .await
    });
    while ops.list().len() < 2 {
        pi_js::time::sleep(5).await;
    }

    harness.session.abort_bash();
    let invs = ops.list();
    let aborted_signals: Vec<bool> = invs
        .iter()
        .map(|i| i.signal.as_ref().is_some_and(|s| s.aborted()))
        .collect();
    for inv in &invs {
        inv.finish();
    }
    let r1 = first_task.await.unwrap().unwrap();
    let r2 = second_task.await.unwrap().unwrap();

    assert_eq!(aborted_signals, vec![true, true]);
    assert_eq!(vec![r1.cancelled, r2.cancelled], vec![true, true]);
    assert!(!harness.session.is_bash_running());
    harness.cleanup();
}

#[tokio::test]
async fn persists_user_assistant_tool_result_and_custom_messages_in_order() {
    let echo_tool = AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "Echo text back".to_string(),
        parameters: t::object(&[t::prop("text", t::string())]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, params, _sig, _upd| {
            Box::pin(async move {
                let text = params.get("text").and_then(Value::as_str).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("echo:{text}")))],
                    details: Some(json!({ "text": text })),
                    structured_content: None,
                    usage: None,
                })
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![echo_tool]),
        ..Default::default()
    })
    .await;
    let mut args = serde_json::Map::new();
    args.insert("text".to_string(), Value::String("hello".to_string()));
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call("echo", args, None))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness
        .session
        .send_custom_message(
            SendCustomMessageDraft {
                custom_type: "note".to_string(),
                content: CustomMessageContent::Text("hello".to_string()),
                display: true,
                details: Some(json!({ "a": 1 })),
            },
            None,
        )
        .await
        .unwrap();
    harness.session.prompt("start", None).await.unwrap();

    assert_eq!(
        entry_types(&harness),
        vec![
            "custom_message",
            "message",
            "message",
            "message",
            "message",
            "message"
        ]
    );
    let roles: Vec<Option<&str>> = harness
        .session
        .messages()
        .iter()
        .map(|m| m.role())
        .collect::<Vec<_>>()
        .into_iter()
        .map(|r| r.map(|_| "")) // keep roles below
        .collect();
    let _ = roles;
    let actual_roles: Vec<String> = harness
        .session
        .messages()
        .into_iter()
        .filter_map(|m| m.role().map(str::to_string))
        .collect();
    assert_eq!(
        actual_roles,
        vec!["custom", "system", "user", "assistant", "toolResult", "assistant"]
    );
    let _ = CustomMessageDelivery::NextTurn;
    harness.cleanup();
}

#[tokio::test]
async fn does_not_emit_message_end_for_bash_execution_messages() {
    let harness = create_harness(HarnessOptions::default()).await;
    let roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let roles_ref = Arc::clone(&roles);
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::MessageEnd { message } = ev
            && let Some(role) = message.role()
        {
            roles_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(role.to_string());
        }
    });

    harness.session.record_bash_result(
        "echo hi",
        &BashResult {
            output: "hi".to_string(),
            exit_code: Some(0),
            cancelled: false,
            truncated: false,
            full_output_path: None,
        },
        None,
    );

    assert!(roles.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn persists_aborted_assistant_messages() {
    let harness = create_harness(HarnessOptions::default()).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("x".repeat(20_000)),
        None,
    ))]);

    let (upd_tx, upd_rx) = tokio::sync::oneshot::channel::<()>();
    let upd_slot = Arc::new(Mutex::new(Some(upd_tx)));
    let _unsub = harness.session.subscribe(move |ev| {
        if matches!(ev, AgentSessionEvent::MessageUpdate { .. })
            && let Some(tx) = upd_slot.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            let _ = tx.send(());
        }
    });

    let sess = harness.session.clone();
    let prompt_task = tokio::spawn(async move { sess.prompt("hi", None).await });
    let _ = upd_rx.await;
    harness.session.abort().await;
    let _ = prompt_task.await;

    let entries = harness.session_manager.get_entries();
    let last = entries.last().unwrap();
    match last {
        pi_coding_agent::core::session_manager::SessionEntry::Message(me) => match &me.message {
            pi_agent_core::types::AgentMessage::Message(pi_ai::types::Message::Assistant(a)) => {
                assert_eq!(a.stop_reason, StopReason::Aborted);
            }
            other => panic!("expected Assistant message, got {other:?}"),
        },
        other => panic!("expected Message entry, got {other:?}"),
    }
    harness.cleanup();
}

#[tokio::test]
async fn records_bash_output_through_custom_operations() {
    let harness = create_harness(HarnessOptions::default()).await;
    let ops: Arc<dyn BashOperations> = Arc::new(FnBashOps(|options: BashExecOptions| {
        Box::pin(async move {
            if let Some(ref on_data) = options.on_data {
                on_data(b"hello from custom ops".to_vec());
            }
            Ok(BashExecResult { exit_code: Some(0) })
        })
    }));

    let res = harness
        .session
        .execute_bash(
            "custom",
            None,
            Some(ExecuteBashOptions {
                operations: Some(ops),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(res.output.contains("hello from custom ops"));
    assert_eq!(harness.session.messages().last().and_then(|m| m.role()), Some("bashExecution"));
    harness.cleanup();
}

#[tokio::test]
async fn streams_bash_output_to_the_callback_and_session_events() {
    let harness = create_harness(HarnessOptions::default()).await;
    let callback_deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let event_updates: Arc<Mutex<Vec<(Option<String>, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let eu_ref = Arc::clone(&event_updates);
    let unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::BashExecutionUpdate { id, delta } = ev {
            eu_ref
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((id, delta));
        }
    });

    let ops: Arc<dyn BashOperations> = Arc::new(FnBashOps(|options: BashExecOptions| {
        Box::pin(async move {
            if let Some(ref on_data) = options.on_data {
                on_data(b"hello ".to_vec());
                on_data(b"world".to_vec());
            }
            Ok(BashExecResult { exit_code: Some(0) })
        })
    }));

    let cb_ref = Arc::clone(&callback_deltas);
    let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |delta| {
        cb_ref
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(delta.to_string());
    });

    harness
        .session
        .execute_bash(
            "custom",
            Some(cb),
            Some(ExecuteBashOptions {
                id: Some("bash-1".to_string()),
                operations: Some(ops),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    unsub.call();

    assert_eq!(
        *callback_deltas.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["hello ".to_string(), "world".to_string()]
    );
    assert_eq!(
        *event_updates.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            (Some("bash-1".to_string()), "hello ".to_string()),
            (Some("bash-1".to_string()), "world".to_string()),
        ]
    );
    harness.cleanup();
}

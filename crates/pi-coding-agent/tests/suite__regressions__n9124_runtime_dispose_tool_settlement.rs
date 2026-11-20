//! Port of packages/coding-agent/test/suite/regressions/9124-runtime-dispose-tool-settlement.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::types::{AgentMessage, AgentTool};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, UserContentBlock};
use pi_coding_agent::core::agent_session_runtime::{
    AgentSessionRuntime, AgentSessionServices, CreateAgentSessionRuntimeFactory,
};
use pi_coding_agent::core::session_manager::SessionEntry;
use pi_js::vendor::typebox::t;
use support::suite::harness::{Harness, HarnessOptions, create_harness};
use tokio::sync::Notify;

async fn create_runtime(tool: AgentTool) -> (Harness, AgentSessionRuntime) {
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![tool]),
        ..Default::default()
    })
    .await;
    let services = AgentSessionServices {
        cwd: harness.temp_dir.clone(),
        agent_dir: harness.temp_dir.clone(),
        model_runtime: harness.session.model_runtime().clone(),
        settings_manager: harness.settings_manager.clone(),
        resource_loader: harness.session.resource_loader().clone(),
        diagnostics: Vec::new(),
    };
    let unused_factory: CreateAgentSessionRuntimeFactory =
        Arc::new(|_| Box::pin(async { Err(pi_js::Error::msg("runtime replacement not used")) }));
    let runtime = AgentSessionRuntime::new(harness.session.clone(), services, unused_factory, Vec::new(), None);
    (harness, runtime)
}

#[tokio::test]
async fn persists_the_aborted_tool_turn_before_disconnecting_the_session() {
    let tool_started = Arc::new(Notify::new());
    let ts_c = Arc::clone(&tool_started);

    let tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait until aborted".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, signal, _upd| {
            let ts_c = Arc::clone(&ts_c);
            Box::pin(async move {
                ts_c.notify_one();
                if let Some(sig) = signal {
                    sig.cancelled().await;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                Err(pi_js::Error::msg("Operation aborted"))
            })
        }),
        replay: None,
    };

    let (harness, runtime) = create_runtime(tool).await;
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "wait",
            Default::default(),
            None,
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);

    let started_wait = tool_started.notified();
    let rt_prompt = runtime.clone();
    let prompt_task = tokio::spawn(async move { rt_prompt.session().prompt("run tool", None).await });
    started_wait.await;

    // #9124: disposal itself must settle persistence without needing to await `prompt_task` first.
    runtime.dispose().await;

    let messages: Vec<AgentMessage> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) if m.message.role() != Some("system") => Some(m.message),
            _ => None,
        })
        .collect();
    let roles: Vec<Option<&str>> = messages.iter().map(|m| m.role()).collect();
    assert_eq!(
        roles,
        vec![Some("user"), Some("assistant"), Some("toolResult"), Some("assistant")]
    );
    let AgentMessage::Message(Message::ToolResult(tr)) = &messages[2] else {
        panic!("expected toolResult");
    };
    assert!(tr.is_error);
    let text = tr
        .content
        .iter()
        .filter_map(|b| match b {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(text, "Operation aborted");

    let _ = prompt_task.await;
    harness.cleanup();
}

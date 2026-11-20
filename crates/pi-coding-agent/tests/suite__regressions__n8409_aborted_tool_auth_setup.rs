//! Port of packages/coding-agent/test/suite/regressions/8409-aborted-tool-auth-setup.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::models::ModelsSimpleStreamOptions;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason};
use pi_js::vendor::typebox::t;
use support::suite::harness::{HarnessOptions, create_harness};
use tokio::sync::Notify;

#[tokio::test]
async fn ends_the_turn_as_aborted_when_auth_setup_sees_an_already_aborted_signal() {
    let tool_started = Arc::new(Notify::new());
    let ts_c = Arc::clone(&tool_started);

    let blocking_tool = AgentTool {
        name: "block".to_string(),
        label: "Block".to_string(),
        description: "Blocks until aborted".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, signal, _upd| {
            let ts_c = Arc::clone(&ts_c);
            Box::pin(async move {
                ts_c.notify_one();
                if let Some(sig) = signal
                    && !sig.aborted()
                {
                    sig.cancelled().await;
                }
                Ok(AgentToolResult::text("tool aborted"))
            })
        }),
        replay: None,
    };

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![blocking_tool]),
        ..Default::default()
    })
    .await;

    let rt = harness.session.model_runtime().clone();
    harness
        .session
        .agent()
        .set_stream_fn(Some(Arc::new(move |model, ctx, opts| {
            let rt = rt.clone();
            Box::pin(async move {
                let simple_opts = opts.map(|base| ModelsSimpleStreamOptions {
                    base,
                    transform_headers: None,
                });
                Ok(rt.stream_simple(&model, &ctx, simple_opts))
            })
        })));

    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
            "block",
            Default::default(),
            None,
        ))]),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    ))]);

    let started_wait = tool_started.notified();
    let s = harness.session.clone();
    let prompt_task = tokio::spawn(async move { s.prompt("run the blocking tool", None).await });
    started_wait.await;
    harness.session.abort().await;
    prompt_task.await.expect("join").expect("prompt");

    let final_assistant = harness
        .session
        .messages()
        .into_iter()
        .rev()
        .find_map(|m| match m {
            AgentMessage::Message(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .expect("Expected final assistant message");
    assert_eq!(final_assistant.stop_reason, StopReason::Aborted);

    harness.cleanup();
}

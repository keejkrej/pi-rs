//! Port of packages/coding-agent/test/suite/regressions/7253-manual-compact-during-response.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::{AgentMessage, AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{Message, StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionEntry,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pi_coding_agent::core::tools::mcp_wire::CompactionResult;
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness};
use tokio::sync::Notify;

fn create_noop_tool() -> AgentTool {
    AgentTool {
        name: "noop".to_string(),
        label: "No-op".to_string(),
        description: "Return immediately".to_string(),
        parameters: t::object(&[]),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_id, _params, _sig, _upd| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("done"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    }
}

#[tokio::test]
async fn persists_the_aborted_response_before_running_the_requested_manual_compaction() {
    let second_response_started = Arc::new(Notify::new());
    let second_response_released = Arc::new(Notify::new());

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeCompactEvent>(|event, _ctx| {
            Box::pin(async move {
                let reason_str = match event.reason {
                    pi_coding_agent::core::extensions::types::CompactionReason::Manual => "manual",
                    pi_coding_agent::core::extensions::types::CompactionReason::Threshold => "threshold",
                    pi_coding_agent::core::extensions::types::CompactionReason::Overflow => "overflow",
                };
                Ok(Some(SessionBeforeCompactResult {
                    compaction: Some(CompactionResult {
                        summary: format!("{reason_str} summary"),
                        first_kept_entry_id: event.preparation.first_kept_entry_id,
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage: None,
                        details: Some(json!({})),
                    }),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(1_000),
            max_tokens: Some(1_000),
            ..Default::default()
        }]),
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(true),
                reserve_tokens: Some(200),
                keep_recent_tokens: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        }),
        tools: Some(vec![create_noop_tool()]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let srs_c = Arc::clone(&second_response_started);
    let srr_c = Arc::clone(&second_response_released);
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call("noop", json!({}))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Factory(Arc::new(move |_ctx, _opts, _st, _mdl| {
            let srs = Arc::clone(&srs_c);
            let srr = Arc::clone(&srr_c);
            Box::pin(async move {
                let wait = srr.notified();
                srs.notify_one();
                wait.await;
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Text(format!("second response:{}", "x".repeat(4_000))),
                    None,
                ))
            })
        })),
    ]);

    let srs_wait = second_response_started.notified();
    let prompt_fut = harness.session.prompt("Run the tool, then continue responding.", None);
    let compact_fut = async {
        srs_wait.await;
        let fut = harness.session.compact(None);
        second_response_released.notify_one();
        fut.await
    };
    let (prompt_res, compact_res) = tokio::join!(prompt_fut, compact_fut);
    prompt_res.unwrap();
    let compact_res = compact_res.unwrap();
    assert_eq!(compact_res.summary, "manual summary");

    let start_reasons: Vec<CompactionReason> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionStart { reason } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(start_reasons, vec![CompactionReason::Manual]);

    let end_reasons: Vec<CompactionReason> = harness
        .events()
        .into_iter()
        .filter_map(|e| match e {
            AgentSessionEvent::CompactionEnd { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(end_reasons, vec![CompactionReason::Manual]);

    let entries = harness.session_manager.get_entries();
    let aborted_response_index = entries.iter().position(|e| match e {
        SessionEntry::Message(m) => match &m.message {
            AgentMessage::Message(Message::Assistant(a)) => a.stop_reason == StopReason::Aborted,
            _ => false,
        },
        _ => false,
    });
    let compaction_index = entries.iter().position(|e| matches!(e, SessionEntry::Compaction(_)));
    let aborted_idx = aborted_response_index.expect("aborted response persisted");
    let comp_idx = compaction_index.expect("compaction entry persisted");
    assert!(comp_idx > aborted_idx);
    assert_eq!(
        entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count(),
        1
    );
    harness.cleanup();
}

//! Port of packages/coding-agent/test/suite/regressions/2023-queued-slash-command-followup.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep,
    faux_assistant_message, faux_tool_call,
};
use pi_ai::types::{StopReason, TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{
    CustomMessageContent, ExtensionApi, InlineExtension, InputDeliveryBehavior, RegisteredCommandOptions,
    SendUserMessageOptions,
};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness, get_assistant_texts, get_user_texts};
use tokio::sync::Notify;

#[tokio::test]
async fn treats_extension_origin_queued_slash_command_follow_ups_as_raw_user_text_instead_of_dispatching_the_command() {
    let api_slot = Arc::new(Mutex::new(Option::<ExtensionApi>::None));
    let command_runs = Arc::new(Mutex::new(Vec::<String>::new()));
    let tool_release = Arc::new(Notify::new());

    let tr_c = Arc::clone(&tool_release);
    let wait_tool = AgentTool {
        name: "wait".to_string(),
        label: "Wait".to_string(),
        description: "Wait for the test to release execution".to_string(),
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]).into(),
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(move |_id, _params, _sig, _upd| {
            let rel = Arc::clone(&tr_c);
            Box::pin(async move {
                rel.notified().await;
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("released"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        replay: None,
    };

    let api_c = Arc::clone(&api_slot);
    let cmd_c = Arc::clone(&command_runs);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let slot = Arc::clone(&api_c);
        let runs = Arc::clone(&cmd_c);
        async move {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(pi.clone());
            pi.register_command(
                "testcmd",
                RegisteredCommandOptions {
                    description: Some("Test command".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |args, _ctx| {
                        let runs = Arc::clone(&runs);
                        Box::pin(async move {
                            runs.lock().unwrap_or_else(PoisonError::into_inner).push(args);
                            Ok(())
                        })
                    }),
                },
            )?;
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        tools: Some(vec![wait_tool]),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![FauxContentBlock::ToolCall(faux_tool_call(
                "wait",
                Default::default(),
                None,
            ))]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("first turn complete".to_string()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("queued follow-up handled by model".to_string()),
            None,
        )),
    ]);

    let saw_tool_start = Arc::new(Notify::new());
    let sts_c = Arc::clone(&saw_tool_start);
    let _unsub = harness.session.subscribe(move |ev| {
        if let AgentSessionEvent::ToolExecutionStart { tool_name, .. } = ev
            && tool_name == "wait"
        {
            sts_c.notify_one();
        }
    });

    let wait_notify = saw_tool_start.notified();
    let prompt_fut = harness.session.prompt("start", None);
    let queue_fut = async {
        wait_notify.await;
        tokio::task::yield_now().await;
        if let Some(api) = api_slot.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            api.send_user_message(
                CustomMessageContent::Text("/testcmd queued".to_string()),
                Some(SendUserMessageOptions {
                    deliver_as: Some(InputDeliveryBehavior::FollowUp),
                    ..Default::default()
                }),
            )
            .unwrap();
        }
        tool_release.notify_one();
    };
    let (prompt_res, ()) = tokio::join!(prompt_fut, queue_fut);
    prompt_res.unwrap();

    assert!(command_runs.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
    assert_eq!(get_user_texts(&harness), vec!["start", "/testcmd queued"]);
    assert!(get_assistant_texts(&harness).contains(&"queued follow-up handled by model".to_string()));
    harness.cleanup();
}

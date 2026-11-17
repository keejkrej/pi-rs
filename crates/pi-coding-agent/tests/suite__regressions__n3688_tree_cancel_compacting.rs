//! Port of packages/coding-agent/test/suite/regressions/3688-tree-cancel-compacting.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::types::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::NavigateTreeOptions;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeTreeEvent, SessionBeforeTreeResult,
};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: pi_js::time::now_ms(),
    }))
}

#[tokio::test]
async fn clears_branch_summary_state_when_session_before_tree_cancels_navigation() {
    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        pi.on::<SessionBeforeTreeEvent>(|_ev, _ctx| {
            Box::pin(async move {
                Ok(Some(SessionBeforeTreeResult {
                    cancel: Some(true),
                    ..Default::default()
                }))
            })
        });
        Ok(())
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let target_id = harness.session_manager.append_message(user_msg("first")).unwrap();
    harness
        .session_manager
        .append_message(AgentMessage::from(Message::Assistant(faux_assistant_message(
            FauxAssistantMessageContent::Text("reply".to_string()),
            None,
        ))))
        .unwrap();
    let current_leaf_id = harness.session_manager.append_message(user_msg("second")).unwrap();

    assert_eq!(harness.session_manager.get_leaf_id().as_deref(), Some(current_leaf_id.as_str()));

    let result = harness
        .session
        .navigate_tree(
            &target_id,
            Some(NavigateTreeOptions {
                summarize: Some(false),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

    assert!(result.cancelled);
    assert!(!harness.session.is_compacting());
    assert_eq!(harness.session_manager.get_leaf_id().as_deref(), Some(current_leaf_id.as_str()));
    harness.cleanup();
}

//! Port of packages/coding-agent/test/suite/regressions/tree-during-streaming.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, FauxResponseStep, faux_assistant_message};
use pi_ai::types::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::NavigateTreeOptions;
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

#[tokio::test]
async fn rejects_tree_navigation_during_active_response_without_changing_leaf() {
    let harness = create_harness(HarnessOptions::default()).await;
    let target_id = harness
        .session_manager
        .append_message(AgentMessage::from(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("first"))]),
            timestamp: pi_js::time::now_ms(),
        })))
        .unwrap();

    let nav_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let leaf_unchanged = Arc::new(AtomicBool::new(false));

    let session_c = harness.session.clone();
    let sm_c = harness.session_manager.clone();
    let target_c = target_id.clone();
    let err_c = Arc::clone(&nav_error);
    let leaf_c = Arc::clone(&leaf_unchanged);

    harness.set_responses(vec![FauxResponseStep::Factory(Arc::new(move |_ctx, _opts, _st, _mdl| {
        let session = session_c.clone();
        let sm = sm_c.clone();
        let target = target_c.clone();
        let err_slot = Arc::clone(&err_c);
        let leaf_slot = Arc::clone(&leaf_c);
        Box::pin(async move {
            let active_leaf_id = sm.get_leaf_id();
            if let Err(err) = session
                .navigate_tree(
                    &target,
                    Some(NavigateTreeOptions {
                        summarize: Some(false),
                        ..Default::default()
                    }),
                )
                .await
            {
                *err_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(err.to_string());
            }
            let same_leaf = active_leaf_id.as_deref() != Some(target.as_str())
                && sm.get_leaf_id() == active_leaf_id;
            leaf_slot.store(same_leaf, Ordering::SeqCst);
            Ok(faux_assistant_message(
                FauxAssistantMessageContent::Text("response".to_string()),
                None,
            ))
        })
    }))]);

    harness.session.prompt("second", None).await.unwrap();

    let recorded_err = nav_error.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(
        recorded_err.as_deref(),
        Some("Wait for the current response to finish before navigating the session tree.")
    );
    assert_eq!(leaf_unchanged.load(Ordering::SeqCst), true);
    harness.cleanup();
}

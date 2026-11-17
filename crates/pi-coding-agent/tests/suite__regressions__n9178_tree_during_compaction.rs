//! Port of packages/coding-agent/test/suite/regressions/9178-tree-during-compaction.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{FauxAssistantMessageContent, faux_assistant_message};
use pi_ai::types::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::agent_session::NavigateTreeOptions;
use pi_coding_agent::core::compaction::CompactionResult;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, SessionBeforeCompactEvent, SessionBeforeCompactResult, SessionBeforeTreeEvent,
    SessionEntry,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness, get_message_text};
use tokio::sync::Notify;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))]),
        timestamp: pi_js::time::now_ms(),
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::from(Message::Assistant(faux_assistant_message(
        FauxAssistantMessageContent::Text(text.to_string()),
        None,
    )))
}

#[tokio::test]
async fn rejects_navigation_while_manual_compaction_is_waiting_before_leaf_changes() {
    let compaction_started = Arc::new(Notify::new());
    let compaction_released = Arc::new(Notify::new());
    let cs_c = Arc::clone(&compaction_started);
    let cr_c = Arc::clone(&compaction_released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let cs = Arc::clone(&cs_c);
        let cr = Arc::clone(&cr_c);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |event, _ctx| {
                let cs = Arc::clone(&cs);
                let cr = Arc::clone(&cr);
                Box::pin(async move {
                    let wait = cr.notified();
                    cs.notify_one();
                    wait.await;
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: None,
                        compaction: Some(CompactionResult {
                            summary: "summary".to_string(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id,
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            details: Some(serde_json::json!({})),
                            usage: None,
                        }),
                    }))
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(CompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    harness.session_manager.append_message(user_msg("first user")).unwrap();
    let navigation_target_id = harness
        .session_manager
        .append_message(assistant_msg("first assistant"))
        .unwrap();
    harness.session_manager.append_message(user_msg("second user")).unwrap();
    let original_leaf_id = harness
        .session_manager
        .append_message(assistant_msg("second assistant"))
        .unwrap();
    let ctx_msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(ctx_msgs);

    let started_wait = compaction_started.notified();
    let session = harness.session.clone();
    let compaction_task = tokio::spawn(async move { session.compact(None).await });
    started_wait.await;

    assert_eq!(harness.session.is_compacting(), true);

    let nav_err = harness
        .session
        .navigate_tree(
            &navigation_target_id,
            Some(NavigateTreeOptions {
                summarize: Some(false),
                ..Default::default()
            }),
        )
        .await
        .expect_err("navigateTree should fail while compaction is active");
    assert!(
        nav_err.to_string().contains(
            "Wait for the current compaction or tree navigation to finish before navigating the session tree."
        ),
        "unexpected error: {nav_err}"
    );
    assert_eq!(
        harness.session_manager.get_leaf_id().as_deref(),
        Some(original_leaf_id.as_str())
    );

    compaction_released.notify_one();
    compaction_task.await.unwrap().unwrap();

    match harness.session_manager.get_entries().last() {
        Some(SessionEntry::Compaction(c)) => {
            assert_eq!(c.parent_id.as_deref(), Some(original_leaf_id.as_str()));
        }
        other => panic!("expected trailing compaction entry, got {other:?}"),
    }
    let texts: Vec<String> = harness.session.messages().iter().map(get_message_text).collect();
    assert!(texts.iter().any(|t| t == "second assistant"));
    harness.cleanup();
}

#[tokio::test]
async fn rejects_second_navigation_while_first_navigation_is_waiting() {
    let nav_started = Arc::new(Notify::new());
    let nav_released = Arc::new(Notify::new());
    let ns_c = Arc::clone(&nav_started);
    let nr_c = Arc::clone(&nav_released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ns = Arc::clone(&ns_c);
        let nr = Arc::clone(&nr_c);
        async move {
            pi.on::<SessionBeforeTreeEvent>(move |_event, _ctx| {
                let ns = Arc::clone(&ns);
                let nr = Arc::clone(&nr);
                Box::pin(async move {
                    let wait = nr.notified();
                    ns.notify_one();
                    wait.await;
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let second_target_id = harness.session_manager.append_message(user_msg("first user")).unwrap();
    let first_target_id = harness
        .session_manager
        .append_message(assistant_msg("first assistant"))
        .unwrap();
    harness.session_manager.append_message(user_msg("second user")).unwrap();
    let original_leaf_id = harness
        .session_manager
        .append_message(assistant_msg("second assistant"))
        .unwrap();
    let ctx_msgs = harness.session_manager.build_session_context().messages;
    harness.session.agent().replace_messages(ctx_msgs);

    let started_wait = nav_started.notified();
    let session = harness.session.clone();
    let target_first = first_target_id.clone();
    let first_nav = tokio::spawn(async move {
        session
            .navigate_tree(
                &target_first,
                Some(NavigateTreeOptions {
                    summarize: Some(false),
                    ..Default::default()
                }),
            )
            .await
    });
    started_wait.await;

    let second_nav = harness.session.navigate_tree(
        &second_target_id,
        Some(NavigateTreeOptions {
            summarize: Some(false),
            ..Default::default()
        }),
    );
    assert_eq!(
        harness.session_manager.get_leaf_id().as_deref(),
        Some(original_leaf_id.as_str())
    );
    nav_released.notify_one();

    let second_err = second_nav
        .await
        .expect_err("second navigateTree should fail while first is running");
    assert!(
        second_err.to_string().contains(
            "Wait for the current compaction or tree navigation to finish before navigating the session tree."
        ),
        "unexpected error: {second_err}"
    );
    first_nav.await.unwrap().unwrap();
    assert_eq!(
        harness.session_manager.get_leaf_id().as_deref(),
        Some(first_target_id.as_str())
    );
    harness.cleanup();
}

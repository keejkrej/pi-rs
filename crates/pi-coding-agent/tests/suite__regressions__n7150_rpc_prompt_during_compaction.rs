//! Port of packages/coding-agent/test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_agent_core::AgentMessage;
use pi_ai::{
    FauxAssistantMessageContent, FauxResponseStep, Message, TextContent, UserContentBlock, UserMessage,
    UserMessageContent, faux_assistant_message,
};
use pi_coding_agent::core::agent_session::{PromptOptions, PromptPreflightOutcome};
use pi_coding_agent::core::compaction::compaction::CompactionResult;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, InputSource, SessionBeforeCompactEvent, SessionBeforeCompactResult,
};
use pi_coding_agent::core::settings_manager::{CompactionSettings, Settings};
use support::suite::harness::{HarnessOptions, create_harness, get_message_text, get_user_texts};
use tokio::sync::Notify;

#[tokio::test]
async fn rejects_an_rpc_prompt_while_manual_compaction_is_in_progress() {
    let compaction_started = Arc::new(Notify::new());
    let compaction_released = Arc::new(Notify::new());
    let started_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let started_ext = Arc::clone(&compaction_started);
    let started_flag_ext = Arc::clone(&started_flag);
    let released_ext = Arc::clone(&compaction_released);

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let started = Arc::clone(&started_ext);
        let started_flag = Arc::clone(&started_flag_ext);
        let released = Arc::clone(&released_ext);
        async move {
            pi.on::<SessionBeforeCompactEvent>(move |ev, _ctx| {
                let started = Arc::clone(&started);
                let started_flag = Arc::clone(&started_flag);
                let released = Arc::clone(&released);
                Box::pin(async move {
                    started_flag.store(true, std::sync::atomic::Ordering::Release);
                    started.notify_waiters();
                    released.notified().await;
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: None,
                        compaction: Some(CompactionResult {
                            summary: "manual compacted".to_string(),
                            first_kept_entry_id: ev.preparation.first_kept_entry_id,
                            tokens_before: ev.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(serde_json::json!({})),
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

    let timestamp = pi_js::time::now_ms() as i64;
    harness
        .session_manager
        .append_message(AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent::new("old user message"))]),
            timestamp: timestamp - 1000,
        })))
        .expect("append user msg");
    harness
        .session_manager
        .append_message(AgentMessage::Message(Message::Assistant(faux_assistant_message(
            FauxAssistantMessageContent::Text("old assistant response".to_string()),
            None,
        ))))
        .expect("append assistant msg");

    let rebuilt = harness.session_manager.build_session_context();
    harness.session.agent().replace_messages(rebuilt.messages);
    harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("probe response".to_string()),
        None,
    ))]);

    let session_for_compact = harness.session.clone();
    let compact_task = tokio::spawn(async move { session_for_compact.compact(None).await });
    while !started_flag.load(std::sync::atomic::Ordering::Acquire) {
        compaction_started.notified().await;
    }

    let preflight_result = Arc::new(Mutex::new(Option::<PromptPreflightOutcome>::None));
    let pf_cb = Arc::clone(&preflight_result);
    let prompt_err = harness
        .session
        .prompt(
            "PROBE-7150",
            Some(PromptOptions {
                source: Some(InputSource::Rpc),
                preflight_result: Some(Arc::new(move |outcome| {
                    *pf_cb.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
                })),
                ..Default::default()
            }),
        )
        .await
        .err();

    compaction_released.notify_waiters();
    compact_task.await.expect("compact join").expect("compact succeeds");

    let persisted_user_texts: Vec<String> = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter_map(|entry| match entry {
            pi_coding_agent::core::session_manager::SessionEntry::Message(m) if m.message.role() == Some("user") => {
                Some(get_message_text(&m.message))
            }
            _ => None,
        })
        .collect();

    assert!(
        preflight_result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    );
    let err = prompt_err.expect("prompt during compaction must fail");
    assert!(
        err.to_string().contains("compaction is in progress"),
        "unexpected error: {err}"
    );
    assert!(!get_user_texts(&harness).contains(&"PROBE-7150".to_string()));
    assert!(!persisted_user_texts.contains(&"PROBE-7150".to_string()));
    assert!(
        !harness
            .events()
            .iter()
            .any(|ev| matches!(ev, pi_coding_agent::modes::json_event::AgentSessionEvent::AgentStart))
    );
    assert!(!harness.events().iter().any(|ev| matches!(
        ev,
        pi_coding_agent::modes::json_event::AgentSessionEvent::AgentSettled { .. }
    )));

    harness.cleanup();
}

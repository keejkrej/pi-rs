//! Port of `packages/coding-agent/test/suite/regressions/7048-compaction-truncated-summary.test.ts`.

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::{AgentMessage, StreamFn};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Message, Model,
    ModelCost, ModelInput, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::{CompactOptions, CompactionSettings, compact, prepare_compaction};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager};
use pi_coding_agent::core::settings_manager::{CompactionSettings as ManagerCompactionSettings, Settings};
use support::suite::harness::{HarnessOptions, create_harness};

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "Test".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

#[tokio::test]
async fn does_not_persist_a_length_limited_summary() {
    let sm = SessionManager::in_memory(None, None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("message to compact".to_owned()),
        timestamp: 1000,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("assistant response to compact"))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "test-model".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 100,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 100,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1500,
    })))
    .unwrap();

    let prep = prepare_compaction(
        &sm.get_branch(None),
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 1,
        },
    )
    .unwrap();

    let stream_fn: StreamFn = Arc::new(|model, _ctx, _opts| {
        Box::pin(async move {
            let stream = AssistantMessageEventStream::new();
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Length,
                message: AssistantMessage {
                    content: vec![AssistantContentBlock::Text(faux_text("partial summar"))],
                    api: model.api,
                    provider: model.provider,
                    model: model.id,
                    response_model: None,
                    response_id: None,
                    provider_thinking_level: None,
                    thinking_level: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Length,
                    deferred: None,
                    error_message: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timestamp: 0,
                },
            });
            Ok(stream)
        })
    });

    let err = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("generation hit the token cap"));
    assert_eq!(
        sm.get_entries()
            .into_iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count(),
        0
    );
}

#[tokio::test]
async fn agent_session_compact_does_not_persist_a_length_limited_summary() {
    let harness = create_harness(HarnessOptions {
        settings: Some(Settings {
            compaction: Some(ManagerCompactionSettings {
                keep_recent_tokens: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("assistant response to compact".to_owned()),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("partial summar".to_owned()),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            }),
        )),
    ]);

    harness.session.prompt("message to compact", None).await.unwrap();
    let err = harness.session.compact(None).await.unwrap_err();
    assert!(err.to_string().contains("generation hit the token cap"), "{err}");
    assert_eq!(
        harness
            .session_manager
            .get_entries()
            .into_iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count(),
        0
    );
    harness.cleanup();
}

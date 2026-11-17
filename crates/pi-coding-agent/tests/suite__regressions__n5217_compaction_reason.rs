//! Port of `packages/coding-agent/test/suite/regressions/5217-compaction-reason.test.ts`.
// WIRE(@ca-cli-w3): full `session_before_compact` / `session_compact` (`reason`, `willRetry`) hook testing belongs in AgentSession once `core::agent_session` lands.

use std::sync::Arc;

use pi_agent_core::{AgentMessage, StreamFn};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Message, Model,
    ModelCost, ModelInput, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::{CompactOptions, CompactionSettings, compact, prepare_compaction};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager};
use tempfile::TempDir;

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

fn mock_assistant(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
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
            output: 20,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 120,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

fn scripted_summary_stream(text: &str) -> StreamFn {
    let summary = text.to_owned();
    Arc::new(move |model, _ctx, _opts| {
        let s = summary.clone();
        Box::pin(async move {
            let mut msg = mock_assistant(&s);
            msg.api = model.api;
            msg.provider = model.provider;
            msg.model = model.id;
            let stream = AssistantMessageEventStream::new();
            stream.push(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: msg,
            });
            Ok(stream)
        })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedCompactionEvent {
    event_type: &'static str,
    reason: &'static str,
    will_retry: bool,
}

#[tokio::test]
async fn compaction_preparation_and_persistence_support_manual_threshold_and_overflow_reasons() {
    let tmp = TempDir::new().unwrap();
    let sm = SessionManager::create(tmp.path().to_str().unwrap(), None, None).unwrap();

    for (prompt, reply) in [("first", "one"), ("second", "two")] {
        sm.append_message(AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Text(prompt.to_owned()),
            timestamp: 1,
        })))
        .unwrap();
        sm.append_message(AgentMessage::Message(Message::Assistant(mock_assistant(reply))))
            .unwrap();
    }

    let entries = sm.get_branch(None);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 1,
    };

    for (reason, will_retry) in [("manual", false), ("threshold", false), ("overflow", true)] {
        let prep = prepare_compaction(&entries, &settings).expect("compactable preparation");
        let res = compact(
            &prep,
            &test_model(),
            Some(CompactOptions {
                stream_fn: Some(scripted_summary_stream("summary from compaction")),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

        let recorded = vec![
            RecordedCompactionEvent {
                event_type: "session_before_compact",
                reason,
                will_retry,
            },
            RecordedCompactionEvent {
                event_type: "session_compact",
                reason,
                will_retry,
            },
        ];
        assert_eq!(recorded.len(), 2);
        assert!(res.summary.contains("summary from compaction"));
        assert!(!res.first_kept_entry_id.is_empty());
    }

    let prep = prepare_compaction(&entries, &settings).unwrap();
    let res = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(scripted_summary_stream("persisted summary")),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    sm.append_compaction(
        &res.summary,
        Some(&res.first_kept_entry_id),
        res.tokens_before,
        res.details
            .map(|d| pi_js::json::parse(&pi_js::json::stringify(&d)).unwrap()),
        None,
        res.usage,
    )
    .unwrap();
    assert_eq!(
        sm.get_entries()
            .into_iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count(),
        1
    );
}

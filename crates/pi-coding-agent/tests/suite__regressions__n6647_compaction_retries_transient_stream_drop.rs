//! Port of `packages/coding-agent/test/suite/regressions/6647-compaction-retries-transient-stream-drop.test.ts`.
// WIRE(@ca-cli-w3): AgentSession wrapper emitting `summarization_retry_scheduled` / `summarization_retry_finished` on SessionEvent is owned by `core::agent_session`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent_core::{AgentMessage, StreamFn};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, Message, Model,
    ModelCost, ModelInput, RetryCallbacks, RetryPolicy, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::compaction::{CompactOptions, CompactionSettings, compact, prepare_compaction};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_js::abort::AbortController;
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

fn make_usage(total_tokens: i64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens,
        cost: Default::default(),
    }
}

fn msg(text: &str, stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
    AssistantMessage {
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![AssistantContentBlock::Text(faux_text(text))]
        },
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "test-model".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: make_usage(10),
        stop_reason,
        deferred: None,
        error_message: error_message.map(ToOwned::to_owned),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }
}

#[derive(Clone, Default)]
struct RecordingCallbacks {
    scheduled: Arc<Mutex<Vec<(u32, u32, String)>>>,
    finished: Arc<Mutex<Vec<(bool, u32, Option<String>)>>>,
}

#[async_trait]
impl RetryCallbacks for RecordingCallbacks {
    async fn on_retry_scheduled(&self, attempt: u32, max_attempts: u32, _delay_ms: u64, error_message: &str) {
        self.scheduled
            .lock()
            .unwrap()
            .push((attempt, max_attempts, error_message.to_owned()));
    }

    async fn on_retry_finished(&self, success: bool, attempt: u32, final_error: Option<&str>) {
        self.finished
            .lock()
            .unwrap()
            .push((success, attempt, final_error.map(ToOwned::to_owned)));
    }
}

fn seed_preparation() -> (TempDir, pi_coding_agent::core::compaction::CompactionPreparation) {
    let tmp = TempDir::new().unwrap();
    let sm = SessionManager::in_memory(Some(tmp.path().to_str().unwrap()), None, None).unwrap();
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text("message to compact".to_owned()),
        timestamp: 1000,
    })))
    .unwrap();
    let mut assistant = msg("assistant response to compact", StopReason::Stop, None);
    assistant.usage = make_usage(100);
    sm.append_message(AgentMessage::Message(Message::Assistant(assistant)))
        .unwrap();

    let entries = sm.get_branch(None);
    let prep = prepare_compaction(
        &entries,
        &CompactionSettings {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 1,
        },
    )
    .unwrap();
    (tmp, prep)
}

fn scripted_stream(script: Vec<AssistantMessage>) -> (StreamFn, Arc<Mutex<usize>>) {
    let call_count = Arc::new(Mutex::new(0usize));
    let count_ref = Arc::clone(&call_count);
    let list = Arc::new(script);

    let stream_fn: StreamFn = Arc::new(move |model, _ctx, _opts| {
        let count_inner = Arc::clone(&count_ref);
        let list_inner = Arc::clone(&list);
        Box::pin(async move {
            let idx = {
                let mut g = count_inner.lock().unwrap();
                let current = *g;
                *g += 1;
                current
            };
            let template = list_inner
                .get(idx)
                .unwrap_or_else(|| list_inner.last().unwrap())
                .clone();
            let mut resp = template;
            resp.api = model.api;
            resp.provider = model.provider;
            resp.model = model.id;

            let stream = AssistantMessageEventStream::new();
            if resp.stop_reason == StopReason::Error || resp.stop_reason == StopReason::Aborted {
                stream.push(AssistantMessageEvent::Error {
                    reason: resp.stop_reason,
                    error: resp,
                });
            } else {
                stream.push(AssistantMessageEvent::Done {
                    reason: resp.stop_reason,
                    message: resp,
                });
            }
            Ok(stream)
        })
    });

    (stream_fn, call_count)
}

#[tokio::test]
async fn retries_transient_terminated_error_and_compacts_successfully() {
    let (_tmp, prep) = seed_preparation();
    let callbacks = Arc::new(RecordingCallbacks::default());
    let (stream_fn, call_count) = scripted_stream(vec![
        msg("", StopReason::Error, Some("terminated")),
        msg("", StopReason::Error, Some("terminated")),
        msg("recovered summary", StopReason::Stop, None),
    ]);

    let result = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            retry: Some(RetryPolicy {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 0,
                max_agent_delay_ms: None,
            }),
            callbacks: Some(callbacks.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    assert!(result.summary.contains("recovered summary"));
    assert_eq!(*call_count.lock().unwrap(), 3);
    let scheduled = callbacks.scheduled.lock().unwrap();
    let finished = callbacks.finished.lock().unwrap();
    assert_eq!(scheduled.len(), 2);
    assert_eq!(scheduled[0], (1, 3, "terminated".to_owned()));
    assert_eq!(scheduled[1], (2, 3, "terminated".to_owned()));
    assert_eq!(finished.len(), 1);
    assert!(finished[0].0);
}

#[tokio::test]
async fn does_not_retry_non_retryable_insufficient_quota() {
    let (_tmp, prep) = seed_preparation();
    let callbacks = Arc::new(RecordingCallbacks::default());
    let (stream_fn, call_count) = scripted_stream(vec![msg("", StopReason::Error, Some("insufficient_quota"))]);

    let err = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            retry: Some(RetryPolicy {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 0,
                max_agent_delay_ms: None,
            }),
            callbacks: Some(callbacks.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("insufficient_quota"));
    assert_eq!(*call_count.lock().unwrap(), 1);
    assert!(callbacks.scheduled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn does_not_retry_when_retry_is_disabled() {
    let (_tmp, prep) = seed_preparation();
    let callbacks = Arc::new(RecordingCallbacks::default());
    let (stream_fn, call_count) = scripted_stream(vec![msg("", StopReason::Error, Some("terminated"))]);

    let err = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            retry: Some(RetryPolicy {
                enabled: false,
                max_retries: 3,
                base_delay_ms: 0,
                max_agent_delay_ms: None,
            }),
            callbacks: Some(callbacks.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("terminated"));
    assert_eq!(*call_count.lock().unwrap(), 1);
    assert!(callbacks.scheduled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stops_retrying_after_max_retries_and_reports_failure() {
    let (_tmp, prep) = seed_preparation();
    let callbacks = Arc::new(RecordingCallbacks::default());
    let (stream_fn, call_count) = scripted_stream(vec![
        msg("", StopReason::Error, Some("terminated")),
        msg("", StopReason::Error, Some("terminated")),
        msg("", StopReason::Error, Some("terminated")),
    ]);

    let err = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            stream_fn: Some(stream_fn),
            retry: Some(RetryPolicy {
                enabled: true,
                max_retries: 2,
                base_delay_ms: 0,
                max_agent_delay_ms: None,
            }),
            callbacks: Some(callbacks.clone()),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("terminated"));
    assert_eq!(*call_count.lock().unwrap(), 3);
    assert_eq!(callbacks.scheduled.lock().unwrap().len(), 2);
    let finished = callbacks.finished.lock().unwrap();
    assert_eq!(finished.len(), 1);
    assert!(!finished[0].0);
}

#[tokio::test]
async fn aborts_in_flight_retry_backoff_via_abort_signal() {
    let (_tmp, prep) = seed_preparation();
    let controller = Arc::new(AbortController::new());
    let signal = controller.signal();
    let (stream_fn, _) = scripted_stream(vec![
        msg("", StopReason::Error, Some("terminated")),
        msg("", StopReason::Error, Some("terminated")),
    ]);

    let ctrl_clone = Arc::clone(&controller);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        ctrl_clone.abort(None);
    });

    let err = compact(
        &prep,
        &test_model(),
        Some(CompactOptions {
            signal: Some(signal),
            stream_fn: Some(stream_fn),
            retry: Some(RetryPolicy {
                enabled: true,
                max_retries: 5,
                base_delay_ms: 30_000,
                max_agent_delay_ms: None,
            }),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();

    assert!(err.is_abort());
}

//! Port of packages/ai/test/event-stream.test.ts (unit ai-utils-errors)

use futures::StreamExt;
use pi_ai::types::{AssistantMessage, AssistantMessageEvent, StopReason, Usage};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, EventStream};

#[tokio::test]
async fn drains_buffered_events_in_order_and_ignores_after_completion() {
    let stream = EventStream::<i32, i32>::new(|e| *e == 3, |e| *e);
    stream.push(1);
    stream.push(2);
    stream.push(3);
    stream.push(4);

    assert_eq!(stream.result().await, Some(3));

    let events: Vec<i32> = stream.collect().await;
    assert_eq!(events, vec![1, 2, 3]);
}

#[tokio::test]
async fn preserves_order_when_events_arrive_after_buffered_draining_starts() {
    let mut stream = EventStream::<i32, i32>::new(|_| false, |e| *e);
    stream.push(1);
    stream.push(2);

    assert_eq!(stream.next().await, Some(1));

    stream.push(3);
    assert_eq!(stream.next().await, Some(2));
    assert_eq!(stream.next().await, Some(3));

    stream.end(Some(3));
    assert_eq!(stream.next().await, None);
    assert_eq!(stream.result().await, Some(3));
}

#[tokio::test]
async fn delivers_events_to_waiting_consumers_in_registration_order() {
    let stream = EventStream::<i32, i32>::new(|_| false, |e| *e);
    let mut first = stream.clone();
    let mut second = stream.clone();

    let first_fut = tokio::spawn(async move { first.next().await });
    tokio::task::yield_now().await;
    let second_fut = tokio::spawn(async move { second.next().await });
    tokio::task::yield_now().await;

    stream.push(1);
    stream.push(2);

    assert_eq!(first_fut.await.unwrap(), Some(1));
    assert_eq!(second_fut.await.unwrap(), Some(2));
}

#[tokio::test]
async fn drains_buffered_events_after_end_and_resolves_explicit_result() {
    let stream = EventStream::<i32, String>::new(|_| false, |e| e.to_string());
    stream.push(1);
    stream.push(2);
    stream.end(Some("complete".to_string()));

    assert_eq!(stream.result().await.as_deref(), Some("complete"));

    let events: Vec<i32> = stream.collect().await;
    assert_eq!(events, vec![1, 2]);
}

#[tokio::test]
async fn wakes_all_waiting_consumers_when_ended_without_result() {
    let stream = EventStream::<i32, i32>::new(|_| false, |e| *e);
    let mut first = stream.clone();
    let mut second = stream.clone();

    let first_fut = tokio::spawn(async move { first.next().await });
    let second_fut = tokio::spawn(async move { second.next().await });
    tokio::task::yield_now().await;

    stream.end(None);

    assert_eq!(first_fut.await.unwrap(), None);
    assert_eq!(second_fut.await.unwrap(), None);
}

fn timing_message(timestamp: i64, duration_ms: Option<i64>) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        model: "m".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms,
    }
}

#[tokio::test]
async fn sets_duration_ms_on_the_final_done_or_error_message_it_saw_start() {
    let done = AssistantMessageEventStream::new();
    let answer = timing_message(pi_js::time::now_ms(), None);
    let _ = pi_js::time::sleep(20, None).await;
    done.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: answer,
    });
    let resolved = done.result().await;
    assert!(resolved.duration_ms.unwrap_or(0) >= 15);

    let failed = AssistantMessageEventStream::new();
    let mut err_msg = timing_message(pi_js::time::now_ms(), None);
    err_msg.stop_reason = StopReason::Error;
    failed.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: err_msg,
    });
    assert!(failed.result().await.duration_ms.is_some());

    let ended = AssistantMessageEventStream::new();
    ended.end(Some(timing_message(pi_js::time::now_ms(), None)));
    assert!(ended.result().await.duration_ms.is_some());
}

#[tokio::test]
async fn keeps_existing_duration_on_forwarding_and_preset_messages() {
    let outer = AssistantMessageEventStream::new();
    let _ = pi_js::time::sleep(20, None).await;
    let inner = AssistantMessageEventStream::new();
    let answer = timing_message(pi_js::time::now_ms(), None);
    inner.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: answer,
    });
    let measured = inner.result().await.duration_ms;
    assert!(measured.unwrap_or(i64::MAX) < 20);
    let forwarded = timing_message(pi_js::time::now_ms(), measured);
    outer.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: forwarded,
    });
    assert_eq!(outer.result().await.duration_ms, measured);

    let preset_stream = AssistantMessageEventStream::new();
    preset_stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: timing_message(pi_js::time::now_ms(), Some(1234)),
    });
    assert_eq!(preset_stream.result().await.duration_ms, Some(1234));
}

#[tokio::test]
async fn leaves_message_untimed_when_started_before_stream_or_pushed_after_completion() {
    let fetched_stream = AssistantMessageEventStream::new();
    fetched_stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: timing_message(pi_js::time::now_ms() - 60_000, None),
    });
    assert_eq!(fetched_stream.result().await.duration_ms, None);

    let completed = AssistantMessageEventStream::new();
    completed.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: timing_message(pi_js::time::now_ms(), None),
    });
    completed.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message: timing_message(pi_js::time::now_ms(), None),
    });
    // First result remains the one captured at completion.
    assert!(completed.result().await.duration_ms.is_some());
}

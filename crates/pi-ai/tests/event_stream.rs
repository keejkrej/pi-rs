//! Port of packages/ai/test/event-stream.test.ts (unit ai-utils-errors)

use futures::StreamExt;
use pi_ai::utils::event_stream::EventStream;

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

//! Port of packages/chord/test/context.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_chord::context::{
    BACKGROUND_CONTEXT, TODO_CONTEXT, await_with_context, create_context_key, with_abort_signal, with_cancel,
    with_context_value, without_abort_signal,
};
use pi_js::abort::{AbortController, AbortReason};

#[test]
fn provides_distinct_empty_root_contexts() {
    let key = create_context_key::<String>("value");

    assert_ne!(TODO_CONTEXT.to_string(), BACKGROUND_CONTEXT.to_string());
    assert!(TODO_CONTEXT.abort_signal().is_none());
    assert_eq!(TODO_CONTEXT.value(&key), None);
    assert_eq!(BACKGROUND_CONTEXT.to_string(), "[Context BACKGROUND_CONTEXT]");
    assert_eq!(TODO_CONTEXT.to_string(), "[Context TODO_CONTEXT]");
}

#[test]
fn layers_typed_values_without_modifying_parents() {
    let first_key = create_context_key::<String>("first");
    let second_key = create_context_key::<i64>("second");
    let first = with_context_value(first_key.clone(), "one".to_string(), BACKGROUND_CONTEXT.clone());
    let second = with_context_value(second_key.clone(), 2, first.clone());
    let replaced = with_context_value(first_key.clone(), "updated".to_string(), second.clone());

    assert_eq!(BACKGROUND_CONTEXT.value(&first_key), None);
    assert_eq!(first.value(&first_key).as_deref(), Some("one"));
    assert_eq!(first.value(&second_key), None);
    assert_eq!(second.value(&first_key).as_deref(), Some("one"));
    assert_eq!(second.value(&second_key), Some(2));
    assert_eq!(replaced.value(&first_key).as_deref(), Some("updated"));
    assert_eq!(second.value(&first_key).as_deref(), Some("one"));
    assert_eq!(
        replaced.to_string(),
        "[Context BACKGROUND_CONTEXT].WithValue(first).WithValue(second).WithValue(first)"
    );
}

#[test]
fn inherits_parent_cancellation_and_isolates_child_cancellation() {
    let parent_controller = AbortController::new();
    let parent = with_abort_signal(parent_controller.signal(), BACKGROUND_CONTEXT.clone());
    let child = with_cancel(parent.clone());
    let sibling = with_cancel(parent.clone());
    let child_calls = Arc::new(AtomicUsize::new(0));
    let child_calls_cb = Arc::clone(&child_calls);
    child
        .context
        .abort_signal()
        .expect("child has abort signal")
        .on_abort(move |_| {
            child_calls_cb.fetch_add(1, Ordering::SeqCst);
        })
        .forget();

    (child.cancel)(Some(AbortReason::value(serde_json::json!("child"))));
    let child_signal = child.context.abort_signal().unwrap();
    let sibling_signal = sibling.context.abort_signal().unwrap();
    assert!(child_signal.aborted());
    assert_eq!(
        child_signal.reason(),
        Some(AbortReason::value(serde_json::json!("child")))
    );
    assert!(!sibling_signal.aborted());
    assert!(!parent.abort_signal().unwrap().aborted());
    assert_eq!(child_calls.load(Ordering::SeqCst), 1);

    parent_controller.abort(Some(AbortReason::value(serde_json::json!("parent"))));
    assert!(sibling_signal.aborted());
    assert_eq!(
        sibling_signal.reason(),
        Some(AbortReason::value(serde_json::json!("parent")))
    );
}

#[test]
fn masks_caller_cancellation_for_mandatory_cleanup() {
    let controller = AbortController::new();
    let key = create_context_key::<String>("value");
    let context = with_context_value(
        key.clone(),
        "preserved".to_string(),
        with_abort_signal(controller.signal(), BACKGROUND_CONTEXT.clone()),
    );
    let cleanup = without_abort_signal(context.clone());

    controller.abort(None);
    assert!(context.abort_signal().unwrap().aborted());
    assert!(cleanup.abort_signal().is_none());
    assert_eq!(cleanup.value(&key).as_deref(), Some("preserved"));
}

#[tokio::test]
async fn stops_waiting_when_the_invocation_is_cancelled() {
    let controller = AbortController::new();
    let context = with_abort_signal(controller.signal(), BACKGROUND_CONTEXT.clone());
    let (tx, rx) = tokio::sync::oneshot::channel::<&'static str>();
    let work = async move { Ok(rx.await.expect("sender dropped")) };
    let waiting = tokio::spawn({
        let context = context.clone();
        async move { await_with_context(work, context).await }
    });
    tokio::task::yield_now().await;

    controller.abort(Some(AbortReason::error("Error", "cancelled")));
    let err = waiting.await.unwrap().expect_err("expected cancellation");
    assert_eq!(err.message(), "cancelled");

    let _ = tx.send("completed later");
    let immediate = await_with_context(async { Ok("completed") }, BACKGROUND_CONTEXT.clone())
        .await
        .unwrap();
    assert_eq!(immediate, "completed");
}

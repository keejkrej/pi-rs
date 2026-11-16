//! Port of packages/durable/test/session-watches.test.ts

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::context::{BACKGROUND_CONTEXT, create_context_key, with_abort_signal, with_cancel, with_context_value};
use pi_chord::delta::{Op, apply_immutable};
use pi_chord::{Context, JsonValue};
use pi_durable::documents::{DocDefinitionInput, SessionDocToken, define_session_doc};
use pi_durable::session::Session;
use pi_durable::types::{WatchEnd, WatchHandle};
use pi_js::abort::{AbortController, AbortReason};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::session_support::{TestSession, context, document_changes, flush, open_test_session};
use tokio::sync::Notify;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Retained {
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    value: i64,
    items: Vec<String>,
    retained: Retained,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SimpleValue {
    value: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MigratedState {
    value: i64,
    migrated: bool,
}

struct Deferred {
    done: std::sync::atomic::AtomicBool,
    notify: Notify,
}

impl Deferred {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: std::sync::atomic::AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn resolve(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        while !self.done.load(Ordering::Acquire) {
            let notified = self.notify.notified();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

fn state_doc() -> SessionDocToken<State> {
    define_session_doc(DocDefinitionInput {
        kind: "watch.state",
        version: 1,
        initial: || State {
            value: 0,
            items: vec!["a".to_owned(), "b".to_owned()],
            retained: Retained {
                label: "stable".to_owned(),
            },
        },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("state doc")
}

async fn create_state() -> (TestSession, SessionDocToken<State>) {
    let harness = open_test_session();
    let doc = state_doc();
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("create state");
    (harness, doc)
}

#[tokio::test]
async fn never_creates_an_absent_document() {
    let harness = open_test_session();
    let doc = state_doc();
    let commits = harness.storage.commit_count();
    assert!(
        harness
            .facade
            .watch_doc_session(&doc, context())
            .await
            .expect("watch")
            .is_none()
    );
    assert_eq!(harness.storage.commit_count(), commits);
    assert_eq!(harness.storage.mint_count(), 0);
}

#[tokio::test]
async fn keeps_the_acquisition_revision_until_start_and_delivers_exact_committed_frames() {
    let (harness, doc) = create_state().await;
    let watch: Arc<WatchHandle<Option<State>>> = Arc::new(
        harness
            .facade
            .watch_doc_session(&doc, context())
            .await
            .expect("watch")
            .expect("present"),
    );
    let initial = watch.value();

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    flush().await;
    let pub1 = harness.last_publication().expect("pub1");
    let first_published = &document_changes(&pub1)[0];

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 2)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 2");
    flush().await;
    let pub2 = harness.last_publication().expect("pub2");
    let second_published = &document_changes(&pub2)[0];
    assert_eq!(watch.value(), initial);

    let inline = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let deliveries: Arc<Mutex<Vec<(Option<State>, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));
    let inline_cb = Arc::clone(&inline);
    let deliveries_cb = Arc::clone(&deliveries);
    let watch_cb = Arc::clone(&watch);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            let inline_cb = Arc::clone(&inline_cb);
            let deliveries_cb = Arc::clone(&deliveries_cb);
            let watch_cb = Arc::clone(&watch_cb);
            Box::pin(async move {
                assert!(!inline_cb.load(Ordering::Acquire));
                assert_eq!(watch_cb.value(), value);
                deliveries_cb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((value, ops));
                Ok(())
            })
        }))
        .expect("start");
    inline.store(false, Ordering::Release);
    assert_eq!(deliveries.lock().unwrap_or_else(|e| e.into_inner()).len(), 0);

    flush().await;
    let recorded = deliveries.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        recorded
            .iter()
            .map(|(v, _)| v.as_ref().map(|s| s.value))
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
    assert_eq!(
        recorded[0].0.as_ref().map(|v| serde_json::to_value(v).expect("j")),
        first_published
            .value
            .as_ref()
            .map(|v| serde_json::to_value(v).expect("j"))
    );
    assert_eq!(recorded[0].1, first_published.ops);
    assert_eq!(
        recorded[1].0.as_ref().map(|v| serde_json::to_value(v).expect("j")),
        second_published
            .value
            .as_ref()
            .map(|v| serde_json::to_value(v).expect("j"))
    );
    assert_eq!(recorded[1].1, second_published.ops);
    assert_eq!(initial.as_ref().map(|s| s.value), Some(0));
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn serializes_callbacks_and_buffers_exact_frames_committed_while_one_is_in_flight() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let entered = Deferred::new();
    let release = Deferred::new();
    let values: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let values_cb = Arc::clone(&values);
    let active_cb = Arc::clone(&active);
    let max_cb = Arc::clone(&max_active);
    watch
        .start(Arc::new(move |value, _ops, _ctx| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let values_cb = Arc::clone(&values_cb);
            let active_cb = Arc::clone(&active_cb);
            let max_cb = Arc::clone(&max_cb);
            Box::pin(async move {
                let cur = active_cb.fetch_add(1, Ordering::SeqCst) + 1;
                max_cb.fetch_max(cur, Ordering::SeqCst);
                let is_first = {
                    let mut guard = values_cb.lock().unwrap_or_else(|e| e.into_inner());
                    guard.push(value.map(|v| v.value).unwrap_or(-1));
                    guard.len() == 1
                };
                if is_first {
                    entered_cb.resolve();
                    release_cb.wait().await;
                }
                active_cb.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    entered.wait().await;

    for v in 2..=20 {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&doc_c).await?.update(|s| s.value = v)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit loop");
    }
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), vec![1]);
    release.resolve();
    flush().await;
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    assert_eq!(
        *values.lock().unwrap_or_else(|e| e.into_inner()),
        (1..=20).collect::<Vec<i64>>()
    );
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn allows_a_listener_to_initiate_a_later_session_commit() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let completed = Deferred::new();
    let values: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let session_cb = Arc::clone(&harness.session);
    let doc_cb = doc.clone();
    let completed_cb = Arc::clone(&completed);
    let values_cb = Arc::clone(&values);
    watch
        .start(Arc::new(move |value, _ops, _ctx| {
            let session_cb = Arc::clone(&session_cb);
            let doc_cb = doc_cb.clone();
            let completed_cb = Arc::clone(&completed_cb);
            let values_cb = Arc::clone(&values_cb);
            Box::pin(async move {
                let current = value.map(|v| v.value).unwrap_or(-1);
                values_cb.lock().unwrap_or_else(|e| e.into_inner()).push(current);
                if current == 1 {
                    session_cb
                        .commit(
                            move |tx| async move {
                                tx.doc_session(&doc_cb).await?.update(|s| s.value = 2)?;
                                Ok(())
                            },
                            context(),
                        )
                        .await?;
                } else {
                    completed_cb.resolve();
                }
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    completed.wait().await;
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), vec![1, 2]);
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn collapses_101_pending_commits_to_one_root_replacement() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    for v in 1..=101 {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&doc_c).await?.update(|s| s.value = v)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit");
    }
    let deliveries: Arc<Mutex<Vec<(i64, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries_cb = Arc::clone(&deliveries);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            deliveries_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((value.map(|v| v.value).unwrap_or(-1), ops));
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");
    flush().await;
    let recorded = deliveries.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, 101);
    assert_eq!(
        serde_json::to_value(&recorded[0].1).expect("json"),
        json!([["r", watch.value()]])
    );
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn never_folds_the_in_flight_frame_into_an_overflow_reset() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let entered = Deferred::new();
    let release = Deferred::new();
    let deliveries: Arc<Mutex<Vec<(i64, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let deliveries_cb = Arc::clone(&deliveries);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let deliveries_cb = Arc::clone(&deliveries_cb);
            Box::pin(async move {
                let first = {
                    let mut guard = deliveries_cb.lock().unwrap_or_else(|e| e.into_inner());
                    guard.push((value.map(|v| v.value).unwrap_or(-1), ops));
                    guard.len() == 1
                };
                if first {
                    entered_cb.resolve();
                    release_cb.wait().await;
                }
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    entered.wait().await;
    for v in 2..=102 {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&doc_c).await?.update(|s| s.value = v)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit loop");
    }
    release.resolve();
    flush().await;
    let recorded = deliveries.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, 1);
    assert_eq!(recorded[1].0, 102);
    assert_eq!(
        serde_json::to_value(&recorded[1].1).expect("json"),
        json!([["r", watch.value()]])
    );
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn folds_retirement_into_an_overflow_reset_and_then_closes() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    for v in 1..=100 {
        let doc_c = doc.clone();
        harness
            .session
            .commit(
                move |tx| async move {
                    tx.doc_session(&doc_c).await?.update(|s| s.value = v)?;
                    Ok(())
                },
                context(),
            )
            .await
            .expect("commit");
    }
    let doc_c = doc.clone();
    harness
        .session
        .commit(move |tx| async move { tx.retire_doc_session(&doc_c).await }, context())
        .await
        .expect("retire");

    let deliveries: Arc<Mutex<Vec<(Option<State>, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries_cb = Arc::clone(&deliveries);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            deliveries_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((value, ops));
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");
    assert_eq!(watch.closed().await, WatchEnd::Retired);
    let recorded = deliveries.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, None);
    assert_eq!(
        serde_json::to_value(&recorded[0].1).expect("json"),
        json!([["r", null]])
    );
}

#[tokio::test]
async fn delivers_replayable_structural_no_op_commits_instead_of_suppressing_them() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let initial = watch.value();

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_session(&doc_c).await?;
                let overlay = draft.overlay()?;
                let items = overlay.get("items")?.as_overlay().cloned().expect("items overlay");
                let first = items.shift()?;
                items.unshift(&[first.into()])?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("shift unshift");

    let newest = harness
        .facade
        .snapshot_session(&doc, context())
        .await
        .expect("snap")
        .expect("present");
    assert_eq!(Some(newest.clone()), initial);

    let batches: Arc<Mutex<Vec<Vec<Op>>>> = Arc::new(Mutex::new(Vec::new()));
    let batches_cb = Arc::clone(&batches);
    let newest_cb = newest;
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            assert_eq!(value.as_ref(), Some(&newest_cb));
            batches_cb.lock().unwrap_or_else(|e| e.into_inner()).push(ops);
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");
    flush().await;
    let recorded = batches.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(recorded.len(), 1);
    assert!(!recorded[0].is_empty());
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn preserves_commit_context_values_without_inheriting_producer_cancellation() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let key = create_context_key::<String>("watch-test");
    let parent_controller = AbortController::new();
    let commit_context = with_context_value(
        key.clone(),
        "newest-commit".to_owned(),
        with_abort_signal(parent_controller.signal(), BACKGROUND_CONTEXT.clone()),
    );
    let entered = Deferred::new();
    let release = Deferred::new();
    let delivery_context: Arc<Mutex<Option<Context>>> = Arc::new(Mutex::new(None));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let ctx_cb = Arc::clone(&delivery_context);
    watch
        .start(Arc::new(move |_value, _ops, delivered| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let ctx_cb = Arc::clone(&ctx_cb);
            Box::pin(async move {
                *ctx_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(delivered);
                entered_cb.resolve();
                release_cb.wait().await;
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            commit_context,
        )
        .await
        .expect("commit");
    entered.wait().await;

    let captured = delivery_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("delivered context");
    assert_eq!(captured.value(&key).as_deref(), Some("newest-commit"));
    assert!(captured.abort_signal().is_none());
    parent_controller.abort(Some(AbortReason::error("AbortError", "caller finished")));
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
    assert!(captured.abort_signal().is_none());
    release.resolve();
}

#[tokio::test]
async fn keeps_earlier_immutable_revisions_stable() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let initial = watch.value();
    let delivered: Arc<Mutex<Option<Option<State>>>> = Arc::new(Mutex::new(None));
    let delivered_cb = Arc::clone(&delivered);
    watch
        .start(Arc::new(move |value, _ops, _ctx| {
            *delivered_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");
    flush().await;

    let d = delivered
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("delivered");
    assert_eq!(d, watch.value());
    assert_ne!(d, initial);
    assert_eq!(d.as_ref().map(|s| &s.retained), initial.as_ref().map(|s| &s.retained));
    assert_eq!(initial.as_ref().map(|s| s.value), Some(0));
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn delivers_retirement_and_does_not_follow_recreation() {
    let (harness, doc) = create_state().await;
    let old_watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("old watch")
        .expect("present");
    let values: Arc<Mutex<Vec<Option<State>>>> = Arc::new(Mutex::new(Vec::new()));
    let values_cb = Arc::clone(&values);
    old_watch
        .start(Arc::new(move |value, _ops, _ctx| {
            values_cb.lock().unwrap_or_else(|e| e.into_inner()).push(value);
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.retire_doc_session(&doc_c).await?;
                tx.doc_session(&doc_c).await?.update(|s| s.value = 10)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("retire & recreate");
    assert_eq!(old_watch.closed().await, WatchEnd::Retired);
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), vec![None]);
    assert_eq!(old_watch.value(), None);

    let replacement = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("replacement")
        .expect("present");
    assert_eq!(replacement.value().as_ref().map(|s| s.value), Some(10));
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 11)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 11");
    flush().await;
    assert_eq!(old_watch.value(), None);
    assert_eq!(replacement.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn session_close_discards_retirement_buffered_before_start() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let baseline = watch.value();
    let doc_c = doc.clone();
    harness
        .session
        .commit(move |tx| async move { tx.retire_doc_session(&doc_c).await }, context())
        .await
        .expect("retire");
    harness.facade.close(context()).await.expect("close");
    assert_eq!(watch.closed().await, WatchEnd::SessionClosed);
    assert_eq!(watch.value(), baseline);
    let err = watch
        .start(Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))))
        .expect_err("start after close");
    assert!(err.to_string().contains("stopped"));
}

#[tokio::test]
async fn session_close_discards_retirement_behind_an_in_flight_callback() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let entered = Deferred::new();
    let release = Deferred::new();
    let values: Arc<Mutex<Vec<Option<i64>>>> = Arc::new(Mutex::new(Vec::new()));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let values_cb = Arc::clone(&values);
    watch
        .start(Arc::new(move |value, _ops, _ctx| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let values_cb = Arc::clone(&values_cb);
            Box::pin(async move {
                values_cb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(value.map(|v| v.value));
                entered_cb.resolve();
                release_cb.wait().await;
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 1");
    entered.wait().await;
    let doc_c = doc.clone();
    harness
        .session
        .commit(move |tx| async move { tx.retire_doc_session(&doc_c).await }, context())
        .await
        .expect("retire");
    harness.facade.close(context()).await.expect("close");
    assert_eq!(watch.closed().await, WatchEnd::SessionClosed);
    release.resolve();
    flush().await;
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), vec![Some(1)]);
}

#[tokio::test]
async fn supports_idempotent_stop_and_rejects_repeated_or_late_start() {
    let (harness, doc) = create_state().await;
    let started = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("started")
        .expect("present");
    started
        .start(Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))))
        .expect("first start");
    let repeated = started
        .start(Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))))
        .expect_err("repeated start");
    assert!(repeated.to_string().contains("already started"));
    let (first, second) = tokio::join!(started.stop(), started.stop());
    assert_eq!(first, WatchEnd::Stopped);
    assert_eq!(second, WatchEnd::Stopped);

    let stopped = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("stopped")
        .expect("present");
    assert_eq!(stopped.stop().await, WatchEnd::Stopped);
    let late = stopped
        .start(Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))))
        .expect_err("late start");
    assert!(late.to_string().contains("stopped"));
}

#[tokio::test]
async fn cancels_acquisition_without_leaking_a_registered_watch() {
    let (harness, doc) = create_state().await;
    harness.facade.unload_documents().await.expect("unload");
    let gate = harness.storage.hold_find_document();
    let child = with_cancel(context());

    let facade_bg = Session::from_impl(Arc::clone(&harness.session));
    let doc_bg = doc.clone();
    let child_ctx = child.context.clone();
    let acquisition = tokio::spawn(async move { facade_bg.watch_doc_session(&doc_bg, child_ctx).await });

    gate.entered().await;
    (child.cancel)(Some(AbortReason::error("Error", "cancel acquisition")));
    gate.release();

    let err = acquisition.await.expect("join").expect_err("acquisition error");
    assert!(err.to_string().contains("cancel acquisition"));
    harness.facade.close(context()).await.expect("close");
}

#[tokio::test]
async fn cancels_future_delivery_without_aborting_an_in_flight_callback() {
    let (harness, doc) = create_state().await;
    let child = with_cancel(context());
    let watch = harness
        .facade
        .watch_doc_session(&doc, child.context.clone())
        .await
        .expect("watch")
        .expect("present");
    let entered = Deferred::new();
    let release = Deferred::new();
    let delivery_context: Arc<Mutex<Option<Context>>> = Arc::new(Mutex::new(None));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let ctx_cb = Arc::clone(&delivery_context);
    watch
        .start(Arc::new(move |_value, _ops, delivered| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let ctx_cb = Arc::clone(&ctx_cb);
            Box::pin(async move {
                *ctx_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(delivered);
                entered_cb.resolve();
                release_cb.wait().await;
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");
    entered.wait().await;
    (child.cancel)(None);
    assert_eq!(watch.closed().await, WatchEnd::Cancelled);
    let captured = delivery_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("captured");
    assert!(captured.abort_signal().is_none());
    release.resolve();
}

#[tokio::test]
async fn session_close_stops_future_delivery_without_joining_an_in_flight_callback() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let entered = Deferred::new();
    let release = Deferred::new();
    let delivery_context: Arc<Mutex<Option<Context>>> = Arc::new(Mutex::new(None));

    let entered_cb = Arc::clone(&entered);
    let release_cb = Arc::clone(&release);
    let ctx_cb = Arc::clone(&delivery_context);
    watch
        .start(Arc::new(move |_value, _ops, delivered| {
            let entered_cb = Arc::clone(&entered_cb);
            let release_cb = Arc::clone(&release_cb);
            let ctx_cb = Arc::clone(&ctx_cb);
            Box::pin(async move {
                *ctx_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(delivered);
                entered_cb.resolve();
                release_cb.wait().await;
                Ok(())
            })
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");
    entered.wait().await;
    harness.facade.close(context()).await.expect("close");
    let captured = delivery_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("captured");
    assert!(captured.abort_signal().is_none());
    assert_eq!(watch.closed().await, WatchEnd::SessionClosed);
    release.resolve();
}

#[tokio::test]
async fn settles_listener_failure_on_only_the_affected_watch() {
    let (harness, doc) = create_state().await;
    let failed = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("failed watch")
        .expect("present");
    let healthy = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("healthy watch")
        .expect("present");

    failed
        .start(Arc::new(|_, _, _| {
            Box::pin(async { Err(pi_js::Error::msg("listener failed")) })
        }))
        .expect("start failed");

    let healthy_calls = Arc::new(AtomicUsize::new(0));
    let healthy_cb = Arc::clone(&healthy_calls);
    healthy
        .start(Arc::new(move |_, _, _| {
            healthy_cb.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start healthy");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 1)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");

    match failed.closed().await {
        WatchEnd::ListenerError { error } => {
            assert_eq!(error.to_string(), "listener failed");
        }
        other => panic!("Expected ListenerError, got {other:?}"),
    }
    flush().await;
    assert_eq!(healthy_calls.load(Ordering::SeqCst), 1);
    assert_eq!(healthy.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn hydrates_migration_without_writing_and_observes_the_later_exact_edit() {
    let old = define_session_doc(DocDefinitionInput {
        kind: "watch.migration",
        version: 1,
        initial: || SimpleValue { value: 3 },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("old");
    let current = define_session_doc(DocDefinitionInput {
        kind: "watch.migration",
        version: 2,
        initial: || MigratedState {
            value: 0,
            migrated: false,
        },
        migrate: Some(|value, _from| {
            let v = value.get("value").and_then(|x| x.as_i64()).unwrap_or(0);
            Ok(MigratedState {
                value: v,
                migrated: true,
            })
        }),
        checkpoint_when: None,
    })
    .expect("current");

    let harness = open_test_session();
    let old_c = old.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&old_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("create old");
    harness.facade.unload_documents().await.expect("unload");
    let commits = harness.storage.commit_count();
    let watch = harness
        .facade
        .watch_doc_session(&current, context())
        .await
        .expect("watch")
        .expect("present");
    assert_eq!(
        watch.value(),
        Some(MigratedState {
            value: 3,
            migrated: true
        })
    );
    assert_eq!(harness.storage.commit_count(), commits);

    let values: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let values_cb = Arc::clone(&values);
    watch
        .start(Arc::new(move |value, _ops, _ctx| {
            values_cb
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(value.map(|v| v.value).unwrap_or(-1));
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");

    let current_c = current.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&current_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("version base");
    flush().await;
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), Vec::<i64>::new());

    let current_c = current.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&current_c).await?.update(|s| s.value = 4)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("update 4");
    flush().await;
    assert_eq!(*values.lock().unwrap_or_else(|e| e.into_inner()), vec![4]);
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn can_replay_every_delivered_exact_operation_batch_from_the_acquisition_revision() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let initial_json = watch.value().map(|v| serde_json::to_value(v).expect("json"));
    let replica: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(initial_json));

    let replica_cb = Arc::clone(&replica);
    watch
        .start(Arc::new(move |value, ops, _ctx| {
            let mut guard = replica_cb.lock().unwrap_or_else(|e| e.into_inner());
            let next: JsonValue = apply_immutable(guard.take(), &ops).expect("replay");
            assert_eq!(next, serde_json::to_value(value).expect("value json"));
            *guard = Some(next);
            Box::pin(futures::future::ready(Ok(())))
        }))
        .expect("start");

    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let draft = tx.doc_session(&doc_c).await?;
                let overlay = draft.overlay()?;
                overlay.set("value", 3_i64)?;
                let items = overlay.get("items")?.as_overlay().cloned().expect("items overlay");
                items.splice(Some(0), Some(1), &[])?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("mutate");
    flush().await;
    assert_eq!(
        *replica.lock().unwrap_or_else(|e| e.into_inner()),
        watch.value().map(|v| serde_json::to_value(v).expect("json"))
    );
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

#[tokio::test]
async fn continues_after_the_tracker_cache_unloads() {
    let (harness, doc) = create_state().await;
    let watch = harness
        .facade
        .watch_doc_session(&doc, context())
        .await
        .expect("watch")
        .expect("present");
    let baseline = watch.value();
    let reads = harness.storage.document_read_count();
    harness.facade.unload_documents().await.expect("unload");
    let reloaded = harness.facade.snapshot_session(&doc, context()).await.expect("snap");
    assert_eq!(reloaded, baseline);
    assert!(harness.storage.document_read_count() > reads);

    watch
        .start(Arc::new(|_, _, _| Box::pin(futures::future::ready(Ok(())))))
        .expect("start");
    let doc_c = doc.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_session(&doc_c).await?.update(|s| s.value = 7)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit 7");
    flush().await;
    assert_eq!(watch.value().as_ref().map(|s| s.value), Some(7));
    assert_eq!(watch.stop().await, WatchEnd::Stopped);
}

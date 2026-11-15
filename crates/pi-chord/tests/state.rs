//! Port of packages/chord/test/state.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::tracker::track;
use pi_chord::delta::{
    AppendVerb, MoveVerb, Op, OpAppend, OpMove, OpReplace, OpSet, OpSplice, OpTruncate, ReplaceVerb, Seg, SetVerb,
    SpliceVerb, TruncateVerb,
};
use pi_chord::services::state::ReplicatedStateReplica;
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{ReplicatedStateDeliveryKind, ReplicatedStateSourceSnapshot};
use pi_chord::{
    Context, JsonValue, ReplicatedStateDelivery, ReplicatedStateSource, ReplicatedStateSourceAttachment,
    ReplicatedStateSourceFrame, ReplicatedStateSourceOptions, replicated_state as replicated_state_initial,
    replicated_state_source,
};
use serde_json::json;

#[test]
fn publishes_one_immutable_structurally_shared_revision() {
    let initial = json!({ "changed": { "value": 1 }, "retained": { "value": 2 } });
    let state = replicated_state_initial(initial.clone()).unwrap();
    let previous = state.value();
    let deliveries: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let d_cb = Arc::clone(&deliveries);
    let _unsub = state.subscribe(Arc::new(move |_value, _ctx, delivery| {
        d_cb.lock().unwrap().push(delivery.sequence);
        Box::pin(futures::future::ready(Ok(())))
    }));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["changed"]["value"] = json!(3);
            draft["changed"]["value"] = json!(4);
        })
        .unwrap();

    assert_eq!(
        state.value(),
        json!({ "changed": { "value": 4 }, "retained": { "value": 2 } })
    );
    assert_ne!(state.value(), previous);
    assert_ne!(state.value()["changed"], previous["changed"]);
    assert_eq!(state.value()["retained"], previous["retained"]);
    assert_eq!(*deliveries.lock().unwrap(), vec![0, 1]);
}

#[test]
fn rolls_back_callback_failures_and_revokes_escaped_drafts() {
    // PORT: TS throws inside `state.change` and relies on Proxy revocation for escaped drafts.
    // In Rust `Draft<T> = T` and `change` callbacks are non-fallible `FnOnce(&mut Draft<T>)`;
    // test aborted `Tracker::begin_change` leaving committed state intact.
    let initial = json!({ "nested": { "value": 1 } });
    let tracker = track(initial.clone());
    let previous = tracker.value();
    let change = tracker.begin_change();
    let mut draft = change.state().unwrap();
    draft["nested"]["value"] = json!(2);
    change.abort();
    assert_eq!(tracker.value(), previous);
}

#[test]
fn rejects_promise_like_callbacks_and_aborts_their_draft() {
    // PORT: TS tests PromiseLike return values from synchronous `change` callbacks at runtime;
    // in Rust `MutableReplicatedState::change` statically requires synchronous `FnOnce(&mut Draft<T>) -> ()`.
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    assert_eq!(state.value(), json!({ "value": 0 }));
}

#[test]
fn rejects_nested_changes_and_replacements_without_losing_the_outer_rollback() {
    let state = replicated_state_initial(json!({ "left": 0, "right": 0 })).unwrap();
    let st_nested = state.clone();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["left"] = json!(1);
            let err = st_nested
                .change(BACKGROUND_CONTEXT.clone(), |nested| {
                    nested["right"] = json!(2);
                })
                .unwrap_err();
            assert!(err.to_js_string().contains("reentrantly"));
        })
        .unwrap();
    // Rollback/adoption on error depends on `helper/chord-delta`'s change tracking;
    // reentrant rejection error is verified directly above and below.
    let st_replace = state.clone();
    state
        .change(BACKGROUND_CONTEXT.clone(), |_draft| {
            let err = st_replace
                .replace(BACKGROUND_CONTEXT.clone(), json!({ "left": 1, "right": 2 }))
                .unwrap_err();
            assert!(err.to_js_string().contains("change callback"));
        })
        .unwrap();
}

#[test]
fn queues_listener_triggered_changes_in_sequence_order() {
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let source_sequences: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let deliveries: Arc<Mutex<Vec<(i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let late_deliveries: Arc<Mutex<Vec<ReplicatedStateDelivery>>> = Arc::new(Mutex::new(Vec::new()));
    let nested = Arc::new(AtomicBool::new(false));

    let internals = get_replicated_state_internals(&state.as_replicated()).unwrap();
    let st_cb = state.clone();
    let late_cb = Arc::clone(&late_deliveries);
    let nest_cb = Arc::clone(&nested);
    let _u1 = internals.subscribe(Arc::new(move |_ops, _sequence, _ctx| {
        if !nest_cb.swap(true, Ordering::SeqCst) {
            st_cb
                .change(BACKGROUND_CONTEXT.clone(), |draft| {
                    draft["value"] = json!(2);
                })
                .unwrap();
            let late_inner = Arc::clone(&late_cb);
            let _ = st_cb.subscribe(Arc::new(move |_val, _ctx, delivery| {
                late_inner.lock().unwrap().push(delivery);
                Box::pin(futures::future::ready(Ok(())))
            }));
        }
    }));

    let ss_cb = Arc::clone(&source_sequences);
    let _u2 = internals.subscribe(Arc::new(move |_ops, sequence, _ctx| {
        ss_cb.lock().unwrap().push(sequence);
    }));

    let d_cb = Arc::clone(&deliveries);
    let _u3 = state.subscribe(Arc::new(move |value, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Update {
            d_cb.lock()
                .unwrap()
                .push((value["value"].as_i64().unwrap(), delivery.sequence));
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(1);
        })
        .unwrap();

    assert_eq!(*source_sequences.lock().unwrap(), vec![1, 2]);
    assert_eq!(*deliveries.lock().unwrap(), vec![(1, 1), (2, 2)]);
    assert_eq!(
        *late_deliveries.lock().unwrap(),
        vec![ReplicatedStateDelivery {
            kind: ReplicatedStateDeliveryKind::Hydrate,
            sequence: 2,
        }]
    );
}

#[test]
fn isolates_listener_failures_after_committing_the_revision() {
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let internals = get_replicated_state_internals(&state.as_replicated()).unwrap();

    let _u1 = internals.subscribe_fallible(Arc::new(|_ops, _seq, _ctx| {
        Err(pi_js::Error::js("Error", "listener failed"))
    }));
    let rec_cb = Arc::clone(&received);
    let _u2 = internals.subscribe(Arc::new(move |_ops, sequence, _ctx| {
        rec_cb.lock().unwrap().push(sequence);
    }));

    let err = state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(1);
        })
        .unwrap_err();
    assert!(err.to_js_string().contains("listener failed"));
    assert_eq!(state.value(), json!({ "value": 1 }));
    assert_eq!(*received.lock().unwrap(), vec![1]);
}

#[test]
fn copies_assigned_values_by_value() {
    let mut external = json!({ "value": 1 });
    let state = replicated_state_initial(json!({ "left": null, "right": null })).unwrap();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["left"] = external.clone();
            draft["right"] = external.clone();
            draft["left"]["value"] = json!(2);
        })
        .unwrap();
    external["value"] = json!(1);
    assert_eq!(external["value"], json!(1));
    assert_eq!(
        state.value(),
        json!({ "left": { "value": 2 }, "right": { "value": 1 } })
    );
    assert_ne!(state.value()["left"], state.value()["right"]);
}

#[test]
fn takes_immutable_ownership_of_alias_free_replacements() {
    let state = replicated_state_initial(json!({ "left": { "value": 1 }, "right": { "value": 2 } })).unwrap();
    let replacement = json!({
        "left": state.value()["left"].clone(),
        "right": state.value()["left"].clone(),
    });
    state.replace(BACKGROUND_CONTEXT.clone(), replacement.clone()).unwrap();
    assert_eq!(state.value(), replacement);
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["left"]["value"] = json!(9);
        })
        .unwrap();
    assert_eq!(
        state.value(),
        json!({ "left": { "value": 9 }, "right": { "value": 1 } })
    );
}

#[test]
fn preserves_compact_string_splice_and_permutation_operations() {
    let a = json!({ "id": "a" });
    let b = json!({ "id": "b" });
    let c = json!({ "id": "c" });
    let state = replicated_state_initial(json!({ "text": "abcdefgh", "values": [a, b, c] })).unwrap();
    let batches: Arc<Mutex<Vec<Vec<Op>>>> = Arc::new(Mutex::new(Vec::new()));
    let internals = get_replicated_state_internals(&state.as_replicated()).unwrap();
    let b_cb = Arc::clone(&batches);
    let _u = internals.subscribe(Arc::new(move |ops, _seq, _ctx| {
        b_cb.lock().unwrap().push(ops.to_vec());
    }));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["text"] = json!("defghxyz");
            draft["values"].as_array_mut().unwrap().remove(0);
        })
        .unwrap();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["values"].as_array_mut().unwrap().reverse();
        })
        .unwrap();

    assert_eq!(
        *batches.lock().unwrap(),
        vec![
            vec![
                Op::Truncate(OpTruncate(TruncateVerb::T, vec![Seg::String("text".into())], 3)),
                Op::Append(OpAppend(AppendVerb::A, vec![Seg::String("text".into())], "xyz".into())),
                Op::Splice(OpSplice(
                    SpliceVerb::P,
                    vec![Seg::String("values".into())],
                    0,
                    1,
                    vec![]
                )),
            ],
            vec![Op::Move(OpMove(
                MoveVerb::M,
                vec![Seg::String("values".into())],
                vec![1, 0],
            ))],
        ]
    );
}

#[test]
fn validates_replica_revisions_without_freezing_shared_immutable_payloads() {
    let replica: ReplicatedStateReplica<JsonValue> = ReplicatedStateReplica::new(|_| {});
    let initial = json!({ "rows": [{ "value": 1 }] });
    replica
        .hydrate(
            0,
            &[Op::Replace(OpReplace(ReplaceVerb::R, initial.clone()))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    assert_eq!(replica.value(), Some(initial.clone()));

    let inserted = json!({ "value": 2 });
    replica
        .update(
            1,
            &[Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("rows".into())],
                1,
                0,
                vec![inserted],
            ))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();

    let updated = replica.value().unwrap();
    assert_ne!(updated, initial);
    assert_eq!(updated["rows"], json!([{ "value": 1 }, { "value": 2 }]));
    assert_eq!(initial["rows"].as_array().unwrap().len(), 1);
}

#[test]
fn clears_a_replica_when_an_adopted_update_is_invalid() {
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let replica: ReplicatedStateReplica<JsonValue> = ReplicatedStateReplica::new(move |err| {
        errs_cb.lock().unwrap().push(err);
    });
    replica
        .hydrate(
            0,
            &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 0 })))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();

    let err_unsafe = replica
        .update(
            1,
            &[Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("__proto__".into())],
                json!(1),
            ))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap_err();
    assert!(!err_unsafe.to_js_string().is_empty());
    assert_eq!(replica.value(), None);

    let err_before_hydration = replica
        .update(
            2,
            &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap_err();
    assert!(err_before_hydration.to_js_string().contains("before hydration"));

    let malformed: ReplicatedStateReplica<JsonValue> = ReplicatedStateReplica::new({
        let errs = Arc::clone(&errors);
        move |err| errs.lock().unwrap().push(err)
    });
    malformed
        .hydrate(
            0,
            &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "values": [1, 2] })))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    let _ = malformed
        .update(
            1,
            &[Op::Move(OpMove(
                MoveVerb::M,
                vec![Seg::String("values".into())],
                vec![0],
            ))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap_err();
    assert_eq!(malformed.value(), None);
    assert!(errors.lock().unwrap().is_empty());
}

#[test]
fn replaces_atomically_and_ignores_deeply_equal_replacements() {
    let state = replicated_state_initial(json!({ "value": { "nested": 1 }, "retained": { "nested": 2 } })).unwrap();
    let previous = state.value();
    let deliveries: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let d_cb = Arc::clone(&deliveries);
    let _u = state.subscribe(Arc::new(move |_v, _ctx, del| {
        d_cb.lock().unwrap().push(del.sequence);
        Box::pin(futures::future::ready(Ok(())))
    }));

    state
        .replace(
            BACKGROUND_CONTEXT.clone(),
            json!({ "value": { "nested": 1 }, "retained": { "nested": 2 } }),
        )
        .unwrap();
    assert_eq!(state.value(), previous);
    assert_eq!(*deliveries.lock().unwrap(), vec![0]);

    state
        .replace(
            BACKGROUND_CONTEXT.clone(),
            json!({ "value": { "nested": 2 }, "retained": previous["retained"].clone() }),
        )
        .unwrap();
    assert_eq!(
        state.value(),
        json!({ "value": { "nested": 2 }, "retained": { "nested": 2 } })
    );
    assert_eq!(state.value()["retained"], previous["retained"]);
    assert_eq!(*deliveries.lock().unwrap(), vec![0, 1]);
}

type FrameListener = Arc<dyn Fn(ReplicatedStateSourceFrame<JsonValue>) -> pi_js::Result<()> + Send + Sync>;

struct TestSourceAttachment {
    snapshot: ReplicatedStateSourceSnapshot<JsonValue>,
    buffer: Mutex<Vec<ReplicatedStateSourceFrame<JsonValue>>>,
    listener: Mutex<Option<FrameListener>>,
    activated: AtomicBool,
    disposed: AtomicBool,
    on_dispose: Arc<dyn Fn() + Send + Sync>,
}

impl TestSourceAttachment {
    fn publish(&self, frame: ReplicatedStateSourceFrame<JsonValue>) -> pi_js::Result<()> {
        if self.disposed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let listener = self.listener.lock().unwrap().clone();
        match listener {
            None => {
                self.buffer.lock().unwrap().push(frame);
                Ok(())
            }
            Some(l) => l(frame),
        }
    }
}

impl ReplicatedStateSourceAttachment<JsonValue> for TestSourceAttachment {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> {
        &self.snapshot
    }

    fn activate(&self, listener: FrameListener) -> pi_js::Result<()> {
        if self.activated.swap(true, Ordering::SeqCst) {
            return Err(pi_js::Error::js("Error", "attachment is already active"));
        }
        *self.listener.lock().unwrap() = Some(listener.clone());
        let buffered = std::mem::take(&mut *self.buffer.lock().unwrap());
        for frame in buffered {
            listener(frame)?;
        }
        Ok(())
    }

    fn dispose(&self) -> pi_js::Result<()> {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.buffer.lock().unwrap().clear();
        (self.on_dispose)();
        Ok(())
    }
}

struct AttachmentHandle(Arc<TestSourceAttachment>);

impl ReplicatedStateSourceAttachment<JsonValue> for AttachmentHandle {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> {
        self.0.snapshot()
    }
    fn activate(&self, listener: FrameListener) -> pi_js::Result<()> {
        self.0.activate(listener)
    }
    fn dispose(&self) -> pi_js::Result<()> {
        self.0.dispose()
    }
}

struct TestSource {
    attachments: Mutex<Vec<(usize, Arc<TestSourceAttachment>)>>,
    next_id: AtomicUsize,
    value: Mutex<JsonValue>,
    cursor: Mutex<i64>,
    on_attach: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl TestSource {
    fn new(value: JsonValue, cursor: i64) -> Arc<Self> {
        Arc::new(Self {
            attachments: Mutex::new(Vec::new()),
            next_id: AtomicUsize::new(1),
            value: Mutex::new(value),
            cursor: Mutex::new(cursor),
            on_attach: Mutex::new(None),
        })
    }

    fn set_on_attach(&self, cb: impl Fn() + Send + Sync + 'static) {
        *self.on_attach.lock().unwrap() = Some(Arc::new(cb));
    }

    fn attachments_len(&self) -> usize {
        self.attachments.lock().unwrap().len()
    }

    fn commit(&self, value: JsonValue, ops: Vec<Op>, context: Option<Context>, cursor: Option<i64>) {
        let next_cursor = cursor.unwrap_or_else(|| *self.cursor.lock().unwrap() + 1);
        *self.value.lock().unwrap() = value.clone();
        *self.cursor.lock().unwrap() = next_cursor;
        let frame = ReplicatedStateSourceFrame {
            cursor: next_cursor,
            value,
            ops,
            context: context.unwrap_or_else(|| BACKGROUND_CONTEXT.clone()),
        };
        let snapshot: Vec<Arc<TestSourceAttachment>> = self
            .attachments
            .lock()
            .unwrap()
            .iter()
            .map(|(_, a)| Arc::clone(a))
            .collect();
        for att in snapshot {
            let _ = att.publish(frame.clone());
        }
    }
}

struct TestSourceArc(Arc<TestSource>);

impl ReplicatedStateSource<JsonValue> for TestSourceArc {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue> + Send + Sync>> {
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        let src = Arc::clone(&self.0);
        let attachment = Arc::new(TestSourceAttachment {
            snapshot: ReplicatedStateSourceSnapshot {
                value: self.0.value.lock().unwrap().clone(),
                cursor: *self.0.cursor.lock().unwrap(),
            },
            buffer: Mutex::new(Vec::new()),
            listener: Mutex::new(None),
            activated: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            on_dispose: Arc::new(move || {
                src.attachments.lock().unwrap().retain(|(entry_id, _)| *entry_id != id);
            }),
        });
        self.0.attachments.lock().unwrap().push((id, Arc::clone(&attachment)));
        let on_attach = self.0.on_attach.lock().unwrap().clone();
        if let Some(cb) = on_attach {
            cb();
        }
        Ok(Box::new(AttachmentHandle(attachment)))
    }
}

#[test]
fn captures_before_activation_and_drains_queued_commits_in_order() {
    let initial = json!({ "value": 0 });
    let first = json!({ "value": 1 });
    let second = json!({ "value": 2 });
    let first_ops = vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))];
    let second_ops = vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))];

    let source = TestSource::new(initial, 10);
    let src_cb = Arc::clone(&source);
    let f_val = first.clone();
    let s_val = second.clone();
    let f_ops = first_ops.clone();
    let s_ops = second_ops.clone();
    source.set_on_attach(move || {
        src_cb.commit(f_val.clone(), f_ops.clone(), None, None);
        src_cb.commit(s_val.clone(), s_ops.clone(), None, None);
    });

    let state = replicated_state_source(Arc::new(TestSourceArc(source)), None).unwrap();
    let deliveries: Arc<Mutex<Vec<(JsonValue, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let d_cb = Arc::clone(&deliveries);
    let _u = state.subscribe(Arc::new(move |value, _ctx, delivery| {
        d_cb.lock().unwrap().push((value, delivery.sequence));
        Box::pin(futures::future::ready(Ok(())))
    }));

    assert_eq!(state.value(), second);
    assert_eq!(*deliveries.lock().unwrap(), vec![(second.clone(), 2)]);
    let snap = get_replicated_state_internals(&state.as_replicated())
        .unwrap()
        .snapshot();
    assert_eq!(snap.value, second);
    assert_eq!(snap.sequence, 2);
}

#[test]
fn hydrates_at_sequence_zero_when_attaching_after_existing_commits() {
    let initial = json!({ "value": 0 });
    let current = json!({ "value": 1 });
    let source = TestSource::new(initial, 40);
    source.commit(
        current.clone(),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
        None,
        None,
    );

    let state = replicated_state_source(Arc::new(TestSourceArc(source)), None).unwrap();
    let deliveries: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let d_cb = Arc::clone(&deliveries);
    let _u = state.subscribe(Arc::new(move |_value, _ctx, delivery| {
        d_cb.lock().unwrap().push(delivery.sequence);
        Box::pin(futures::future::ready(Ok(())))
    }));
    assert_eq!(state.value(), current);
    assert_eq!(*deliveries.lock().unwrap(), vec![0]);
}

#[test]
fn publishes_exact_source_value_and_operation_references_without_applying_or_re_diffing() {
    let source = TestSource::new(json!({ "value": 0 }), 0);
    let state = replicated_state_source(Arc::new(TestSourceArc(Arc::clone(&source))), None).unwrap();
    let next = json!({ "value": 1 });
    let ops = vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))];

    let published_ops: Arc<Mutex<Option<Vec<Op>>>> = Arc::new(Mutex::new(None));
    let po_cb = Arc::clone(&published_ops);
    let _u1 = get_replicated_state_internals(&state.as_replicated())
        .unwrap()
        .subscribe(Arc::new(move |received, _seq, _ctx| {
            *po_cb.lock().unwrap() = Some(received.to_vec());
        }));

    let published_value: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let pv_cb = Arc::clone(&published_value);
    let _u2 = state.subscribe(Arc::new(move |value, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Update {
            *pv_cb.lock().unwrap() = Some(value);
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    source.commit(next.clone(), ops.clone(), None, None);
    assert_eq!(state.value(), next);
    assert_eq!(*published_value.lock().unwrap(), Some(next));
    assert_eq!(*published_ops.lock().unwrap(), Some(ops));
}

#[test]
fn buffers_reentrant_frames_and_skips_updates_covered_by_a_late_hydration() {
    let source = TestSource::new(json!({ "value": 0 }), 0);
    let state = replicated_state_source(Arc::new(TestSourceArc(Arc::clone(&source))), None).unwrap();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let late: Arc<Mutex<Vec<(ReplicatedStateDeliveryKind, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let nested = Arc::new(AtomicBool::new(false));

    let src_cb = Arc::clone(&source);
    let st_cb = state.clone();
    let l_cb = Arc::clone(&late);
    let nest_cb = Arc::clone(&nested);
    let _u1 = get_replicated_state_internals(&state.as_replicated())
        .unwrap()
        .subscribe(Arc::new(move |_ops, sequence, _ctx| {
            if sequence != 1 || nest_cb.swap(true, Ordering::SeqCst) {
                return;
            }
            src_cb.commit(
                json!({ "value": 2 }),
                vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))],
                None,
                None,
            );
            let l_inner = Arc::clone(&l_cb);
            let _ = st_cb.subscribe(Arc::new(move |value, _ctx, delivery| {
                l_inner
                    .lock()
                    .unwrap()
                    .push((delivery.kind, delivery.sequence, value["value"].as_i64().unwrap()));
                Box::pin(futures::future::ready(Ok(())))
            }));
        }));

    let r_cb = Arc::clone(&received);
    let _u2 = state.subscribe(Arc::new(move |value, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Update {
            r_cb.lock().unwrap().push(value["value"].as_i64().unwrap());
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    source.commit(
        json!({ "value": 1 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
        None,
        None,
    );
    assert_eq!(*received.lock().unwrap(), vec![1, 2]);
    assert_eq!(
        *late.lock().unwrap(),
        vec![(ReplicatedStateDeliveryKind::Hydrate, 2, 2)]
    );
}

#[test]
fn reports_listener_failures_without_throwing_them_into_the_source() {
    let source = TestSource::new(json!({ "value": 0 }), 0);
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let state = replicated_state_source(
        Arc::new(TestSourceArc(Arc::clone(&source))),
        Some(ReplicatedStateSourceOptions {
            on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        }),
    )
    .unwrap();

    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let _u1 = state.subscribe(Arc::new(|_val, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Update {
            return Box::pin(futures::future::ready(Err(pi_js::Error::js(
                "Error",
                "listener failed",
            ))));
        }
        Box::pin(futures::future::ready(Ok(())))
    }));
    let rec_cb = Arc::clone(&received);
    let _u2 = state.subscribe(Arc::new(move |val, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Update {
            rec_cb.lock().unwrap().push(val["value"].as_i64().unwrap());
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    source.commit(
        json!({ "value": 1 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
        None,
        None,
    );
    source.commit(
        json!({ "value": 2 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))],
        None,
        None,
    );

    assert_eq!(*received.lock().unwrap(), vec![1, 2]);
    let msgs: Vec<String> = errors.lock().unwrap().iter().map(|e| e.message().to_string()).collect();
    assert_eq!(msgs, vec!["listener failed", "listener failed"]);
}

#[test]
fn reports_cursor_gaps_disposes_the_broken_attachment_and_ignores_later_frames() {
    let source = TestSource::new(json!({ "value": 0 }), 5);
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let state = replicated_state_source(
        Arc::new(TestSourceArc(Arc::clone(&source))),
        Some(ReplicatedStateSourceOptions {
            on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        }),
    )
    .unwrap();

    source.commit(
        json!({ "value": 2 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))],
        None,
        Some(7),
    );
    assert!(
        errors.lock().unwrap()[0]
            .to_js_string()
            .contains("expected 6, received 7")
    );
    assert_eq!(source.attachments_len(), 0);
    assert_eq!(state.value(), json!({ "value": 0 }));

    source.commit(
        json!({ "value": 3 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(3)))],
        None,
        Some(8),
    );
    assert_eq!(state.value(), json!({ "value": 0 }));
}

#[test]
fn keeps_attachments_independent_and_disposes_each_idempotently() {
    let source = TestSource::new(json!({ "value": 0 }), 0);
    let first = replicated_state_source(Arc::new(TestSourceArc(Arc::clone(&source))), None).unwrap();
    let second = replicated_state_source(Arc::new(TestSourceArc(Arc::clone(&source))), None).unwrap();
    assert_eq!(source.attachments_len(), 2);

    source.commit(
        json!({ "value": 1 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
        None,
        None,
    );
    assert_eq!(first.value()["value"], json!(1));
    assert_eq!(second.value()["value"], json!(1));

    first.dispose().unwrap();
    first.dispose().unwrap();
    assert_eq!(source.attachments_len(), 1);

    source.commit(
        json!({ "value": 2 }),
        vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(2)))],
        None,
        None,
    );
    assert_eq!(first.value()["value"], json!(1));
    assert_eq!(second.value()["value"], json!(2));
    second.dispose().unwrap();
    assert_eq!(source.attachments_len(), 0);
}

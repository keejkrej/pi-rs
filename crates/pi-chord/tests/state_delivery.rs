//! Port of packages/chord/test/state-delivery.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use pi_chord::context::{BACKGROUND_CONTEXT, with_cancel};
use pi_chord::delta::{Op, OpReplace, OpSet, ReplaceVerb, Seg, SetVerb};
use pi_chord::services::state::ReplicatedStateReplica;
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{ReplicatedStateDeliveryKind, ReplicatedStateSourceSnapshot};
use pi_chord::{
    Context, JsonValue, ReplicatedState, ReplicatedStateDelivery, ReplicatedStateSource,
    ReplicatedStateSourceAttachment, ReplicatedStateSourceFrame, ReplicatedStateSourceOptions,
    replicated_state as replicated_state_initial, replicated_state_source,
};
use serde_json::json;

#[derive(Clone, Copy)]
enum Kind {
    Mutable,
    Attached,
    Replica,
}

#[derive(Clone)]
struct Deferred {
    tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<Result<(), (String, String)>>>>>,
    fut: futures::future::Shared<pi_js::BoxFuture<Result<(), (String, String)>>>,
}

impl Deferred {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), (String, String)>>();
        let fut: pi_js::BoxFuture<Result<(), (String, String)>> = Box::pin(async move {
            match rx.await {
                Ok(res) => res,
                Err(_) => Ok(()),
            }
        });
        Self {
            tx: Arc::new(Mutex::new(Some(tx))),
            fut: fut.shared(),
        }
    }

    fn resolve(&self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Ok(()));
        }
    }

    fn reject(&self, error: pi_js::Error) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let pair = match error {
                pi_js::Error::Js(js) => (js.name, js.message),
                other => ("Error".to_string(), other.message().to_string()),
            };
            let _ = tx.send(Err(pair));
        }
    }

    fn wait(&self) -> impl std::future::Future<Output = pi_js::Result<()>> + Send + 'static {
        let fut = self.fut.clone();
        async move { fut.await.map_err(|(name, message)| pi_js::Error::js(&name, message)) }
    }
}

enum FixtureState {
    Replicated(ReplicatedState<JsonValue>),
    Replica(ReplicatedStateReplica<JsonValue>),
}

impl FixtureState {
    fn value(&self) -> Option<JsonValue> {
        match self {
            Self::Replicated(s) => s.value(),
            Self::Replica(r) => r.value(),
        }
    }

    fn subscribe(
        &self,
        listener: Arc<
            dyn Fn(JsonValue, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync,
        >,
    ) -> pi_js::Unsubscribe {
        match self {
            Self::Replicated(s) => s.subscribe(listener),
            Self::Replica(r) => r.subscribe(move |v, ctx, del| listener(v, ctx, del)),
        }
    }

    fn internals(&self) -> Option<Arc<dyn pi_chord::services::state_internals::ReplicatedStateInternals>> {
        match self {
            Self::Replicated(s) => get_replicated_state_internals(s),
            Self::Replica(_) => None,
        }
    }
}

struct Fixture {
    state: FixtureState,
    publish: Box<dyn Fn(JsonValue, Context) + Send + Sync>,
}

fn fixture(kind: Kind, on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>) -> Fixture {
    match kind {
        Kind::Mutable => {
            let mutable = replicated_state_initial(json!({ "value": 0 })).unwrap();
            let state = FixtureState::Replicated(mutable.as_replicated());
            let publish = Box::new(move |value, context| {
                mutable.replace(context, value).unwrap();
            });
            Fixture { state, publish }
        }
        Kind::Attached => {
            type FrameCb = Arc<dyn Fn(ReplicatedStateSourceFrame<JsonValue>) -> pi_js::Result<()> + Send + Sync>;
            let receive: Arc<Mutex<Option<FrameCb>>> = Arc::new(Mutex::new(None));
            struct Att(Arc<Mutex<Option<FrameCb>>>, ReplicatedStateSourceSnapshot<JsonValue>);
            impl ReplicatedStateSourceAttachment<JsonValue> for Att {
                fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> {
                    &self.1
                }
                fn activate(&self, listener: FrameCb) -> pi_js::Result<()> {
                    *self.0.lock().unwrap() = Some(listener);
                    Ok(())
                }
                fn dispose(&self) -> pi_js::Result<()> {
                    Ok(())
                }
            }
            struct Src(Arc<Mutex<Option<FrameCb>>>);
            impl ReplicatedStateSource<JsonValue> for Src {
                fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue> + Send + Sync>> {
                    Ok(Box::new(Att(
                        Arc::clone(&self.0),
                        ReplicatedStateSourceSnapshot {
                            value: json!({ "value": 0 }),
                            cursor: 0,
                        },
                    )))
                }
            }
            let attached = replicated_state_source(
                Arc::new(Src(Arc::clone(&receive))),
                on_error.map(|cb| ReplicatedStateSourceOptions { on_error: Some(cb) }),
            )
            .unwrap();
            let state = FixtureState::Replicated(attached.as_replicated());
            let publish = Box::new(move |value: JsonValue, context: Context| {
                let cursor = value["value"].as_i64().unwrap();
                let l = receive.lock().unwrap().clone().unwrap();
                l(ReplicatedStateSourceFrame {
                    cursor,
                    value: value.clone(),
                    ops: vec![Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("value".into())],
                        json!(cursor),
                    ))],
                    context,
                })
                .unwrap();
            });
            Fixture { state, publish }
        }
        Kind::Replica => {
            let replica = match on_error {
                Some(cb) => ReplicatedStateReplica::new(move |e| cb(e)),
                None => ReplicatedStateReplica::new(|_| {}),
            };
            replica
                .hydrate(
                    0,
                    &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 0 })))],
                    BACKGROUND_CONTEXT.clone(),
                )
                .unwrap();
            let r_pub = replica.clone();
            let state = FixtureState::Replica(replica);
            let publish = Box::new(move |value: JsonValue, context: Context| {
                let seq = value["value"].as_i64().unwrap();
                r_pub
                    .update(
                        seq,
                        &[Op::Set(OpSet(
                            SetVerb::S,
                            vec![Seg::String("value".into())],
                            json!(seq),
                        ))],
                        context,
                    )
                    .unwrap();
            });
            Fixture { state, publish }
        }
    }
}

async fn wait_for(cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not satisfied in time");
}

async fn run_awaits_hydration_and_each_update_independently(kind: Kind) {
    let f = fixture(kind, None);
    let hydration = Deferred::new();
    let update = Deferred::new();
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let exact: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let fast: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let _exact_sub = f.state.internals().map(|i| {
        let ex = Arc::clone(&exact);
        i.subscribe(Arc::new(move |_ops, seq, _ctx| {
            ex.lock().unwrap().push(seq);
        }))
    });

    let ev_cb = Arc::clone(&events);
    let hyd_cb = hydration.clone();
    let upd_cb = update.clone();
    let _s1 = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let ev = Arc::clone(&ev_cb);
        let h = hyd_cb.clone();
        let u = upd_cb.clone();
        Box::pin(async move {
            let v = value["value"].as_i64().unwrap();
            ev.lock().unwrap().push(format!("start:{v}"));
            if v == 0 {
                h.wait().await?;
            }
            if v == 1 {
                u.wait().await?;
            }
            ev.lock().unwrap().push(format!("end:{v}"));
            Ok(())
        })
    }));

    let fast_cb = Arc::clone(&fast);
    let _s2 = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        fast_cb.lock().unwrap().push(value["value"].as_i64().unwrap());
        Box::pin(futures::future::ready(Ok(())))
    }));

    (f.publish)(json!({ "value": 1 }), BACKGROUND_CONTEXT.clone());
    (f.publish)(json!({ "value": 2 }), BACKGROUND_CONTEXT.clone());

    assert_eq!(*events.lock().unwrap(), vec!["start:0"]);
    assert_eq!(*fast.lock().unwrap(), vec![0, 1, 2]);
    assert_eq!(f.state.value(), Some(json!({ "value": 2 })));
    if !matches!(kind, Kind::Replica) {
        assert_eq!(*exact.lock().unwrap(), vec![1, 2]);
    }

    hydration.resolve();
    wait_for(|| *events.lock().unwrap() == vec!["start:0", "end:0", "start:1"]).await;
    update.resolve();
    wait_for(|| *events.lock().unwrap() == vec!["start:0", "end:0", "start:1", "end:1", "start:2", "end:2"]).await;
}

async fn run_bounds_pending_deliveries(kind: Kind, count: i64) {
    let f = fixture(kind, None);
    let hydration = Deferred::new();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let exact: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let _exact_sub = f.state.internals().map(|i| {
        let ex = Arc::clone(&exact);
        i.subscribe(Arc::new(move |_ops, seq, _ctx| {
            ex.lock().unwrap().push(seq);
        }))
    });

    let rec_cb = Arc::clone(&received);
    let hyd_cb = hydration.clone();
    let _s = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let v = value["value"].as_i64().unwrap();
        rec_cb.lock().unwrap().push(v);
        if v == 0 {
            let h = hyd_cb.clone();
            return Box::pin(async move { h.wait().await });
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    for value in 1..=count {
        (f.publish)(json!({ "value": value }), BACKGROUND_CONTEXT.clone());
    }
    assert_eq!(*received.lock().unwrap(), vec![0]);
    if !matches!(kind, Kind::Replica) {
        assert_eq!(*exact.lock().unwrap(), (1..=count).collect::<Vec<_>>());
    }

    hydration.resolve();
    let first = ((count - 1) / 100) * 100 + 1;
    let mut expected = vec![0];
    expected.extend(first..=count);
    wait_for(|| *received.lock().unwrap() == expected).await;
}

async fn run_excludes_running_update_from_overflow(kind: Kind) {
    let f = fixture(kind, None);
    let gate = Deferred::new();
    let context = with_cancel(BACKGROUND_CONTEXT.clone()).context;
    let received: Arc<Mutex<Vec<(JsonValue, Context, ReplicatedStateDelivery)>>> = Arc::new(Mutex::new(Vec::new()));

    let rec_cb = Arc::clone(&received);
    let gate_cb = gate.clone();
    let _s = f.state.subscribe(Arc::new(move |value, ctx, delivery| {
        let v = value["value"].as_i64().unwrap();
        rec_cb.lock().unwrap().push((value, ctx, delivery));
        if v == 1 {
            let g = gate_cb.clone();
            return Box::pin(async move { g.wait().await });
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    for value in 1..102 {
        (f.publish)(json!({ "value": value }), BACKGROUND_CONTEXT.clone());
    }
    let newest = json!({ "value": 102 });
    (f.publish)(newest.clone(), context.clone());
    let adopted = f.state.value().unwrap();
    (f.publish)(json!({ "value": 103 }), BACKGROUND_CONTEXT.clone());

    assert_eq!(
        received
            .lock()
            .unwrap()
            .iter()
            .map(|(v, _, _)| v["value"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    gate.resolve();
    wait_for(|| {
        received
            .lock()
            .unwrap()
            .iter()
            .map(|(v, _, _)| v["value"].as_i64().unwrap())
            .collect::<Vec<_>>()
            == vec![0, 1, 102, 103]
    })
    .await;

    let r = received.lock().unwrap();
    assert_eq!(r[2].0, adopted);
    assert!(r[2].1.abort_signal().is_some());
    assert_eq!(
        r[2].2,
        ReplicatedStateDelivery {
            kind: ReplicatedStateDeliveryKind::Update,
            sequence: 102,
        }
    );
    assert_eq!(context.abort_signal().unwrap().aborted(), false);
}

fn run_serializes_reentrant_callbacks(kind: Kind) {
    let f = Arc::new(fixture(kind, None));
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let f_cb = Arc::clone(&f);
    let ev_cb = Arc::clone(&events);
    let _s = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let v = value["value"].as_i64().unwrap();
        ev_cb.lock().unwrap().push(format!("start:{v}"));
        if v < 2 {
            (f_cb.publish)(json!({ "value": v + 1 }), BACKGROUND_CONTEXT.clone());
        }
        ev_cb.lock().unwrap().push(format!("end:{v}"));
        Box::pin(futures::future::ready(Ok(())))
    }));

    assert_eq!(
        *events.lock().unwrap(),
        vec!["start:0", "end:0", "start:1", "end:1", "start:2", "end:2"]
    );
}

async fn run_treats_two_subscriptions_independently(kind: Kind) {
    let f = fixture(kind, None);
    let gate = Deferred::new();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let make_listener = || {
        let rec = Arc::clone(&received);
        let g = gate.clone();
        Arc::new(move |value: JsonValue, _ctx: Context, _del: ReplicatedStateDelivery| {
            let v = value["value"].as_i64().unwrap();
            rec.lock().unwrap().push(v);
            if v == 0 {
                let g2 = g.clone();
                return Box::pin(async move { g2.wait().await }) as pi_js::BoxFuture<pi_js::Result<()>>;
            }
            Box::pin(futures::future::ready(Ok(())))
        })
    };

    let stop_first = f.state.subscribe(make_listener());
    let stop_second = f.state.subscribe(make_listener());
    (f.publish)(json!({ "value": 1 }), BACKGROUND_CONTEXT.clone());
    stop_first.call();
    gate.resolve();
    wait_for(|| *received.lock().unwrap() == vec![0, 0, 1]).await;
    stop_second.call();
}

async fn run_unsubscribe_drops_queued_without_aborting_running(kind: Kind) {
    let f = fixture(kind, None);
    let gate = Deferred::new();
    let context = with_cancel(BACKGROUND_CONTEXT.clone()).context;
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));

    let rec = Arc::clone(&received);
    let g = gate.clone();
    let comp = Arc::clone(&completed);
    let stop = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let r = Arc::clone(&rec);
        let gate_inner = g.clone();
        let c = Arc::clone(&comp);
        Box::pin(async move {
            let v = value["value"].as_i64().unwrap();
            r.lock().unwrap().push(v);
            if v == 1 {
                gate_inner.wait().await?;
                c.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }));

    tokio::task::yield_now().await;
    (f.publish)(json!({ "value": 1 }), context.clone());
    (f.publish)(json!({ "value": 2 }), context.clone());
    stop.call();
    (f.publish)(json!({ "value": 3 }), context.clone());
    assert!(!context.abort_signal().unwrap().aborted());
    assert!(!completed.load(Ordering::SeqCst));
    gate.resolve();
    wait_for(|| completed.load(Ordering::SeqCst)).await;
    assert_eq!(*received.lock().unwrap(), vec![0, 1]);
}

fn run_isolates_sync_hydration_failure(kind: Kind) {
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let f = fixture(kind, Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))));
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    let _s = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let v = value["value"].as_i64().unwrap();
        rec.lock().unwrap().push(v);
        if v == 0 {
            return Box::pin(futures::future::ready(Err(pi_js::Error::js("Error", "sync hydration"))));
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    (f.publish)(json!({ "value": 1 }), BACKGROUND_CONTEXT.clone());
    assert_eq!(
        errors.lock().unwrap().iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec!["sync hydration"]
    );
    assert_eq!(*received.lock().unwrap(), vec![0, 1]);
}

async fn run_observes_hydration_and_update_errors_while_continuing_delivery(kind: Kind) {
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let f = fixture(kind, Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))));
    let gate = Deferred::new();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let fast: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let rec = Arc::clone(&received);
    let g = gate.clone();
    let _s1 = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        let v = value["value"].as_i64().unwrap();
        rec.lock().unwrap().push(v);
        if v == 0 {
            let g2 = g.clone();
            return Box::pin(async move { g2.wait().await });
        }
        if v == 1 {
            return Box::pin(futures::future::ready(Err(pi_js::Error::js("Error", "sync update"))));
        }
        if v == 2 {
            return Box::pin(async { Err(pi_js::Error::js("Error", "async update")) });
        }
        Box::pin(futures::future::ready(Ok(())))
    }));

    let fast_cb = Arc::clone(&fast);
    let _s2 = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        fast_cb.lock().unwrap().push(value["value"].as_i64().unwrap());
        Box::pin(futures::future::ready(Ok(())))
    }));

    for value in 1..=3 {
        (f.publish)(json!({ "value": value }), BACKGROUND_CONTEXT.clone());
    }
    gate.reject(pi_js::Error::js("Error", "async hydration"));
    wait_for(|| *received.lock().unwrap() == vec![0, 1, 2, 3]).await;
    assert_eq!(
        errors.lock().unwrap().iter().map(|e| e.message()).collect::<Vec<_>>(),
        vec!["async hydration", "sync update", "async update"]
    );
    assert_eq!(*fast.lock().unwrap(), vec![0, 1, 2, 3]);
}

async fn run_observes_callback_rejection_after_unsubscribe(kind: Kind) {
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let f = fixture(kind, Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))));
    let gate = Deferred::new();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));

    let rec = Arc::clone(&received);
    let g = gate.clone();
    let stop = f.state.subscribe(Arc::new(move |value, _ctx, _del| {
        rec.lock().unwrap().push(value["value"].as_i64().unwrap());
        let g2 = g.clone();
        Box::pin(async move { g2.wait().await })
    }));

    (f.publish)(json!({ "value": 1 }), BACKGROUND_CONTEXT.clone());
    stop.call();
    gate.reject(pi_js::Error::js("Error", "stopped callback"));
    wait_for(|| errors.lock().unwrap().len() == 1).await;
    assert_eq!(errors.lock().unwrap()[0].message(), "stopped callback");
    assert_eq!(*received.lock().unwrap(), vec![0]);
}

#[tokio::test]
async fn mutable_awaits_hydration_and_updates_independently() {
    run_awaits_hydration_and_each_update_independently(Kind::Mutable).await;
}

#[tokio::test]
async fn attached_awaits_hydration_and_updates_independently() {
    run_awaits_hydration_and_each_update_independently(Kind::Attached).await;
}

#[tokio::test]
async fn replica_awaits_hydration_and_updates_independently() {
    run_awaits_hydration_and_each_update_independently(Kind::Replica).await;
}

#[tokio::test]
async fn mutable_bounds_pending_deliveries() {
    for count in [100, 101, 102, 201, 202] {
        run_bounds_pending_deliveries(Kind::Mutable, count).await;
    }
}

#[tokio::test]
async fn attached_bounds_pending_deliveries() {
    for count in [100, 101, 102, 201, 202] {
        run_bounds_pending_deliveries(Kind::Attached, count).await;
    }
}

#[tokio::test]
async fn replica_bounds_pending_deliveries() {
    for count in [100, 101, 102, 201, 202] {
        run_bounds_pending_deliveries(Kind::Replica, count).await;
    }
}

#[tokio::test]
async fn mutable_excludes_running_update_from_overflow() {
    run_excludes_running_update_from_overflow(Kind::Mutable).await;
}

#[tokio::test]
async fn attached_excludes_running_update_from_overflow() {
    run_excludes_running_update_from_overflow(Kind::Attached).await;
}

#[tokio::test]
async fn replica_excludes_running_update_from_overflow() {
    run_excludes_running_update_from_overflow(Kind::Replica).await;
}

#[test]
fn mutable_serializes_reentrant_callbacks() {
    run_serializes_reentrant_callbacks(Kind::Mutable);
}

#[test]
fn attached_serializes_reentrant_callbacks() {
    run_serializes_reentrant_callbacks(Kind::Attached);
}

#[test]
fn replica_serializes_reentrant_callbacks() {
    run_serializes_reentrant_callbacks(Kind::Replica);
}

#[tokio::test]
async fn mutable_treats_two_subscriptions_independently() {
    run_treats_two_subscriptions_independently(Kind::Mutable).await;
}

#[tokio::test]
async fn attached_treats_two_subscriptions_independently() {
    run_treats_two_subscriptions_independently(Kind::Attached).await;
}

#[tokio::test]
async fn replica_treats_two_subscriptions_independently() {
    run_treats_two_subscriptions_independently(Kind::Replica).await;
}

#[tokio::test]
async fn mutable_unsubscribe_drops_queued_callbacks() {
    run_unsubscribe_drops_queued_without_aborting_running(Kind::Mutable).await;
}

#[tokio::test]
async fn attached_unsubscribe_drops_queued_callbacks() {
    run_unsubscribe_drops_queued_without_aborting_running(Kind::Attached).await;
}

#[tokio::test]
async fn replica_unsubscribe_drops_queued_callbacks() {
    run_unsubscribe_drops_queued_without_aborting_running(Kind::Replica).await;
}

#[test]
fn attached_isolates_sync_hydration_failure() {
    run_isolates_sync_hydration_failure(Kind::Attached);
}

#[test]
fn replica_isolates_sync_hydration_failure() {
    run_isolates_sync_hydration_failure(Kind::Replica);
}

#[tokio::test]
async fn attached_observes_listener_errors_and_continues() {
    run_observes_hydration_and_update_errors_while_continuing_delivery(Kind::Attached).await;
}

#[tokio::test]
async fn replica_observes_listener_errors_and_continues() {
    run_observes_hydration_and_update_errors_while_continuing_delivery(Kind::Replica).await;
}

#[tokio::test]
async fn attached_observes_rejection_after_unsubscribe() {
    run_observes_callback_rejection_after_unsubscribe(Kind::Attached).await;
}

#[tokio::test]
async fn replica_observes_rejection_after_unsubscribe() {
    run_observes_callback_rejection_after_unsubscribe(Kind::Replica).await;
}

#[tokio::test]
async fn mutable_state_reports_rejected_callbacks_through_its_default_error_reporter() {
    // PORT: `MutableReplicatedStateImpl` delegates listener errors to `report_error_async`
    // (`pi_js::console::error`), so `replace` does not fail and subsequent updates continue delivering.
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    let _s = state.subscribe(Arc::new(move |value, _ctx, _del| {
        let v = value["value"].as_i64().unwrap();
        rec.lock().unwrap().push(v);
        if v == 0 {
            return Box::pin(async { Err(pi_js::Error::js("Error", "async mutable hydration")) });
        }
        Box::pin(async { Ok(()) })
    }));

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(*received.lock().unwrap(), vec![0, 1]);
}

#[tokio::test]
async fn replica_disconnect_drops_obsolete_pending_work_but_waits_for_the_running_callback_before_rehydration() {
    let replica: ReplicatedStateReplica<JsonValue> = ReplicatedStateReplica::new(|_| {});
    let gate = Deferred::new();
    let received: Arc<Mutex<Vec<ReplicatedStateDelivery>>> = Arc::new(Mutex::new(Vec::new()));

    let rec = Arc::clone(&received);
    let g = gate.clone();
    let _s = replica.subscribe(move |_val, _ctx, delivery| {
        rec.lock().unwrap().push(delivery);
        if delivery.sequence == 0 {
            let g2 = g.clone();
            return Box::pin(async move { g2.wait().await });
        }
        Box::pin(futures::future::ready(Ok(())))
    });

    replica
        .hydrate(
            0,
            &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 0 })))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    replica
        .update(
            1,
            &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    replica.clear();
    replica
        .hydrate(
            50,
            &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 50 })))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    replica
        .update(
            51,
            &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(51)))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();

    assert_eq!(
        *received.lock().unwrap(),
        vec![ReplicatedStateDelivery {
            kind: ReplicatedStateDeliveryKind::Hydrate,
            sequence: 0,
        }]
    );
    gate.resolve();
    wait_for(|| {
        *received.lock().unwrap()
            == vec![
                ReplicatedStateDelivery {
                    kind: ReplicatedStateDeliveryKind::Hydrate,
                    sequence: 0,
                },
                ReplicatedStateDelivery {
                    kind: ReplicatedStateDeliveryKind::Hydrate,
                    sequence: 50,
                },
                ReplicatedStateDelivery {
                    kind: ReplicatedStateDeliveryKind::Update,
                    sequence: 51,
                },
            ]
    })
    .await;
}

#[test]
fn cold_replicas_hydrate_all_listeners_before_their_reentrant_updates() {
    let replica: ReplicatedStateReplica<JsonValue> = ReplicatedStateReplica::new(|_| {});
    let second: Arc<Mutex<Vec<ReplicatedStateDelivery>>> = Arc::new(Mutex::new(Vec::new()));

    let r_cb = replica.clone();
    let _s1 = replica.subscribe(move |_val, _ctx, delivery| {
        if delivery.kind == ReplicatedStateDeliveryKind::Hydrate {
            r_cb.update(
                1,
                &[Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
                BACKGROUND_CONTEXT.clone(),
            )
            .unwrap();
        }
        Box::pin(futures::future::ready(Ok(())))
    });
    let sec_cb = Arc::clone(&second);
    let _s2 = replica.subscribe(move |_val, _ctx, delivery| {
        sec_cb.lock().unwrap().push(delivery);
        Box::pin(futures::future::ready(Ok(())))
    });

    replica
        .hydrate(
            0,
            &[Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 0 })))],
            BACKGROUND_CONTEXT.clone(),
        )
        .unwrap();
    assert_eq!(
        *second.lock().unwrap(),
        vec![
            ReplicatedStateDelivery {
                kind: ReplicatedStateDeliveryKind::Hydrate,
                sequence: 0,
            },
            ReplicatedStateDelivery {
                kind: ReplicatedStateDeliveryKind::Update,
                sequence: 1,
            },
        ]
    );
}

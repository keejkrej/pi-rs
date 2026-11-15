//! Port of packages/chord/test/service-delivery.test.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_chord::context::{BACKGROUND_CONTEXT, with_cancel};
use pi_chord::delta::{Op, OpReplace, ReplaceVerb};
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::provider::{RemoteServiceProviderEntry, RemoteServiceSubscription};
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{ServiceIdRef, ServiceMemberStateKind, ServiceStateSnapshot};
use pi_chord::{
    Context, JsonValue, MutableReplicatedState, RemoteServiceBindingOptions, RemoteServiceProvider,
    RemoteServiceTransport, Service, ServiceInstanceSnapshot, ServiceMemberSnapshot, ServiceMode,
    ServiceProviderUpdate, ServiceSubscription, ServiceSubscriptionSnapshot, create_remote_service_binding,
    create_service_state_decoder, create_service_state_encoder, define_service, parse_wire_service_provider_update,
    parse_wire_service_subscription_snapshot, replicated_state as replicated_state_initial,
};
use serde_json::json;

fn svc_counter() -> Service {
    define_service("test.delivery-counter", None).unwrap()
}

fn svc_pair() -> Service {
    define_service("test.delivery-pair", None).unwrap()
}

struct MutableStateTarget(MutableReplicatedState<JsonValue>);

impl ServiceTarget for MutableStateTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        match property {
            PropertyKey::String(key) if key == "value" => Ok(ServiceValue::from_json(&self.0.value())),
            _ => Ok(ServiceValue::Undefined),
        }
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "ReplicatedState is not callable"))
    }
}

fn wrap_mutable_state(state: &MutableReplicatedState<JsonValue>) -> ServiceObject {
    let internals =
        get_replicated_state_internals(&state.as_replicated()).expect("mutable state internals must be registered");
    ServiceObject::from_state(MutableStateTarget(state.clone()), internals)
}

fn counter_impl(state: &MutableReplicatedState<JsonValue>) -> Arc<ServiceObject> {
    let mut members = IndexMap::new();
    members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(state)));
    Arc::new(ServiceObject::from_members(members))
}

fn pair_impl(
    left: &MutableReplicatedState<JsonValue>,
    right: &MutableReplicatedState<JsonValue>,
) -> Arc<ServiceObject> {
    let mut members = IndexMap::new();
    members.insert("left".to_string(), ServiceValue::Object(wrap_mutable_state(left)));
    members.insert("right".to_string(), ServiceValue::Object(wrap_mutable_state(right)));
    Arc::new(ServiceObject::from_members(members))
}

fn get_state_value(service: &ServiceObject, member: &str) -> pi_js::Result<Option<JsonValue>> {
    let ServiceValue::Object(state_obj) = service.get_key(member)? else {
        panic!("expected state member {member}");
    };
    match state_obj.get_key("value")? {
        ServiceValue::Undefined => Ok(None),
        other => Ok(Some(other.to_json()?)),
    }
}

fn subscribe_state_values(
    service: &ServiceObject,
    member: &str,
    on_value: impl Fn(JsonValue) + Send + Sync + 'static,
) -> pi_js::Result<ServiceObject> {
    let ServiceValue::Object(state_obj) = service.get_key(member)? else {
        panic!("expected state member {member}");
    };
    let ServiceValue::Object(sub_fn) = state_obj.get_key("subscribe")? else {
        panic!("expected subscribe member");
    };
    let cb = Arc::new(on_value);
    let listener = ServiceObject::from_fn(move |_this, args| {
        if let Some(val) = args.first() {
            cb(val.to_json()?);
        }
        Ok(ServiceValue::Undefined)
    });
    match sub_fn.apply(None, &[ServiceValue::Object(listener)])? {
        ServiceValue::Object(unsub) => Ok(unsub),
        _ => panic!("expected unsubscribe object"),
    }
}

fn wire_transport(
    provider: RemoteServiceProvider,
    before_activate: Arc<dyn Fn() + Send + Sync>,
    updates: Arc<Mutex<Vec<ServiceProviderUpdate>>>,
) -> RemoteServiceTransport {
    let prov_invoke = provider.clone();
    RemoteServiceTransport::new(
        Arc::new(move |call, context| {
            let p = prov_invoke.clone();
            let c = call.clone();
            let ctx = context.clone();
            Box::pin(async move { p.invoke(&c, ctx).await })
        }),
        Arc::new(move |service_id, mode, listener, _context| {
            let p = provider.clone();
            let ba = Arc::clone(&before_activate);
            let ups = Arc::clone(&updates);
            let sid = service_id.to_string();
            Box::pin(async move {
                let encoder = Arc::new(Mutex::new(create_service_state_encoder()));
                let decoder = Arc::new(Mutex::new(create_service_state_decoder()));
                let enc_sub = Arc::clone(&encoder);
                let dec_sub = Arc::clone(&decoder);
                let subscription = p.subscribe(&sid, mode, move |update, ctx| {
                    let wire_upd = enc_sub.lock().unwrap().encode_update(&update).unwrap();
                    let parsed = parse_wire_service_provider_update(&serde_json::to_value(wire_upd).unwrap()).unwrap();
                    let decoded = dec_sub.lock().unwrap().decode_update(&parsed).unwrap();
                    ups.lock().unwrap().push(decoded.clone());
                    listener(decoded, ctx);
                })?;
                let wire_snap = encoder
                    .lock()
                    .unwrap()
                    .encode_snapshot(subscription.snapshot())
                    .unwrap();
                let parsed_snap =
                    parse_wire_service_subscription_snapshot(&serde_json::to_value(wire_snap).unwrap()).unwrap();
                let snapshot = decoder.lock().unwrap().decode_snapshot(&parsed_snap).unwrap();

                let sub_act = subscription.clone();
                let sub_cls = subscription;
                Ok(ServiceSubscription::new(
                    snapshot,
                    Arc::new(move || {
                        ba();
                        sub_act.activate()
                    }),
                    Arc::new(move |ctx| {
                        let s = sub_cls.clone();
                        Box::pin(async move { s.close(ctx) })
                    }),
                ))
            })
        }),
    )
}

#[test]
fn appends_reentrant_updates_to_the_activation_fifo() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&state)).unwrap();

    let received: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    let st_cb = state.clone();
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            let ServiceProviderUpdate::State { sequence, .. } = update else {
                return;
            };
            rec.lock().unwrap().push(sequence);
            if sequence == 1 {
                st_cb
                    .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 3 }))
                    .unwrap();
            }
        })
        .unwrap();

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 2 }))
        .unwrap();
    subscription.activate().unwrap();
    subscription.activate().unwrap();
    assert_eq!(*received.lock().unwrap(), vec![1, 2, 3]);
    provider.dispose().unwrap();
}

#[test]
fn bounds_pending_updates_with_explicit_root_resets_100() {
    run_bounds_pending_updates_with_last_check(100);
}

#[test]
fn bounds_pending_updates_with_explicit_root_resets_101() {
    run_bounds_pending_updates_with_last_check(101);
}

#[test]
fn bounds_pending_updates_with_explicit_root_resets_102() {
    run_bounds_pending_updates_with_last_check(102);
}

#[test]
fn bounds_pending_updates_with_explicit_root_resets_201() {
    run_bounds_pending_updates_with_last_check(201);
}

#[test]
fn bounds_pending_updates_with_explicit_root_resets_202() {
    run_bounds_pending_updates_with_last_check(202);
}

fn run_bounds_pending_updates_with_last_check(count: i64) {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&state)).unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let contexts: Arc<Mutex<Vec<Context>>> = Arc::new(Mutex::new(Vec::new()));
    let ups = Arc::clone(&updates);
    let ctxs = Arc::clone(&contexts);
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, ctx| {
            ups.lock().unwrap().push(update);
            ctxs.lock().unwrap().push(ctx);
        })
        .unwrap();

    let wc = with_cancel(BACKGROUND_CONTEXT.clone());
    let context = wc.context;
    for value in 1..=count {
        state.replace(context.clone(), json!({ "value": value })).unwrap();
    }
    assert!(updates.lock().unwrap().is_empty());
    match &subscription.snapshot().instances[0].members[0] {
        ServiceMemberSnapshot::State(s) => assert_eq!(s.sequence, 0),
        _ => panic!(),
    }
    subscription.activate().unwrap();
    let first = ((count - 1) / 100) * 100 + 1;
    {
        let captured = updates.lock().unwrap();
        assert_eq!(captured.len(), (count - first + 1) as usize);
        if count > 100 {
            assert_eq!(
                captured[0],
                ServiceProviderUpdate::Reset {
                    snapshot: ServiceSubscriptionSnapshot {
                        service_id: counter.id.clone(),
                        mode: ServiceMode::Singleton,
                        instances: vec![ServiceInstanceSnapshot {
                            instance: None,
                            members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                                name: "state".to_string(),
                                kind: ServiceMemberStateKind::State,
                                sequence: first,
                                ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": first })))],
                            })],
                        }],
                    }
                }
            );
        }
    }
    for received in contexts.lock().unwrap().iter() {
        assert!(received.abort_signal().is_some());
    }
    state.replace(context, json!({ "value": count + 1 })).unwrap();
    match updates.lock().unwrap().last() {
        Some(ServiceProviderUpdate::State { sequence, .. }) => assert_eq!(*sequence, count + 1),
        other => panic!("expected final state update, got {other:?}"),
    }
    provider.dispose().unwrap();
}

#[test]
fn does_not_compact_the_running_delivery_when_activation_overflows_reentrantly() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&state)).unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let ups = Arc::clone(&updates);
    let st_cb = state.clone();
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            ups.lock().unwrap().push(update.clone());
            if let ServiceProviderUpdate::State { sequence, .. } = update {
                if sequence == 1 {
                    for value in 3..=103 {
                        st_cb
                            .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": value }))
                            .unwrap();
                    }
                }
            }
        })
        .unwrap();

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 2 }))
        .unwrap();
    subscription.activate().unwrap();

    {
        let captured = updates.lock().unwrap();
        assert_eq!(captured.len(), 3);
        match &captured[0] {
            ServiceProviderUpdate::State { sequence, .. } => assert_eq!(*sequence, 1),
            other => panic!("unexpected first: {other:?}"),
        }
        match &captured[1] {
            ServiceProviderUpdate::Reset { snapshot } => match &snapshot.instances[0].members[0] {
                ServiceMemberSnapshot::State(s) => {
                    assert_eq!(s.sequence, 102);
                    assert_eq!(
                        s.ops,
                        vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 102 })))]
                    );
                }
                _ => panic!(),
            },
            other => panic!("unexpected second: {other:?}"),
        }
        match &captured[2] {
            ServiceProviderUpdate::State { sequence, .. } => assert_eq!(*sequence, 103),
            other => panic!("unexpected third: {other:?}"),
        }
    }
    provider.dispose().unwrap();
}

#[test]
fn suppresses_notifications_already_covered_by_an_overflow_snapshot() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let internals = get_replicated_state_internals(&state.as_replicated()).unwrap();
    let st_cb = state.clone();
    let _unsub = internals.subscribe(Arc::new(move |_ops, sequence, _ctx| {
        if sequence == 101 {
            st_cb
                .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 102 }))
                .unwrap();
            st_cb
                .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 103 }))
                .unwrap();
        }
    }));

    provider.provide(&counter, counter_impl(&state)).unwrap();
    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let ups = Arc::clone(&updates);
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            ups.lock().unwrap().push(update);
        })
        .unwrap();

    for value in 1..=101 {
        state
            .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": value }))
            .unwrap();
    }
    subscription.activate().unwrap();
    {
        let captured = updates.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ServiceProviderUpdate::Reset { snapshot } => match &snapshot.instances[0].members[0] {
                ServiceMemberSnapshot::State(s) => {
                    assert_eq!(s.sequence, 103);
                    assert_eq!(
                        s.ops,
                        vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 103 })))]
                    );
                }
                _ => panic!(),
            },
            other => panic!("unexpected update: {other:?}"),
        }
    }

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 104 }))
        .unwrap();
    match &updates.lock().unwrap()[1] {
        ServiceProviderUpdate::State { sequence, .. } => assert_eq!(*sequence, 104),
        other => panic!("unexpected second update: {other:?}"),
    }
    provider.dispose().unwrap();
}

#[test]
fn preserves_lifecycle_ordering_across_subscribers_during_reentrant_publication() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let s0 = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&s0)).unwrap();

    let first: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let second: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));

    let f_cb = Arc::clone(&first);
    let prov_cb = provider.clone();
    let ctr_cb = counter.clone();
    provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            let len = {
                let mut g = f_cb.lock().unwrap();
                g.push(update);
                g.len()
            };
            if len == 1 {
                let s2 = replicated_state_initial(json!({ "value": 2 })).unwrap();
                prov_cb.replace(&ctr_cb, counter_impl(&s2)).unwrap();
            }
        })
        .unwrap()
        .activate()
        .unwrap();

    let s_cb = Arc::clone(&second);
    provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            s_cb.lock().unwrap().push(update);
        })
        .unwrap()
        .activate()
        .unwrap();

    let s1 = replicated_state_initial(json!({ "value": 1 })).unwrap();
    provider.replace(&counter, counter_impl(&s1)).unwrap();

    {
        let f_vec = first.lock().unwrap();
        let s_vec = second.lock().unwrap();
        assert_eq!(*f_vec, *s_vec);
        assert_eq!(s_vec.len(), 2);
        match &s_vec[0] {
            ServiceProviderUpdate::Replaced { snapshot } => match &snapshot.members[0] {
                ServiceMemberSnapshot::State(s) => {
                    assert_eq!(
                        s.ops,
                        vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 1 })))]
                    );
                }
                _ => panic!(),
            },
            other => panic!("unexpected update: {other:?}"),
        }
    }
    provider.dispose().unwrap();
}

#[test]
fn close_during_a_callback_prevents_later_buffered_callbacks() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&state)).unwrap();

    let received: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let sub_slot: Arc<Mutex<Option<RemoteServiceSubscription>>> = Arc::new(Mutex::new(None));
    let rec = Arc::clone(&received);
    let ss = Arc::clone(&sub_slot);
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            rec.lock().unwrap().push(update);
            if let Some(sub) = ss.lock().unwrap().as_ref() {
                sub.close(None).unwrap();
            }
        })
        .unwrap();
    *sub_slot.lock().unwrap() = Some(subscription.clone());

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 2 }))
        .unwrap();
    subscription.activate().unwrap();
    assert_eq!(received.lock().unwrap().len(), 1);
    provider.dispose().unwrap();
}

#[test]
fn drains_terminal_updates_when_disposed_reentrantly() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&state)).unwrap();

    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    let prov_cb = provider.clone();
    let subscription = provider
        .subscribe(&counter.id, ServiceMode::Singleton, move |update, _ctx| {
            let kind = match update {
                ServiceProviderUpdate::State { .. } => "state",
                ServiceProviderUpdate::Reset { .. } => "reset",
                ServiceProviderUpdate::Unavailable => "unavailable",
                ServiceProviderUpdate::Replaced { .. } => "replaced",
                ServiceProviderUpdate::Spawned { .. } => "spawned",
                ServiceProviderUpdate::Closed { .. } => "closed",
            };
            rec.lock().unwrap().push(kind.to_string());
            if kind == "state" {
                prov_cb.dispose().unwrap();
            }
        })
        .unwrap();

    state
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    subscription.activate().unwrap();
    assert_eq!(*received.lock().unwrap(), vec!["state", "unavailable"]);
}

#[tokio::test]
async fn rebaselines_every_member_through_wire_codecs_then_resumes_contiguous_deltas() {
    let pair_svc = svc_pair();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(pair_svc.clone())]).unwrap();
    let left = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let right = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&pair_svc, pair_impl(&left, &right)).unwrap();

    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let l_ba = left.clone();
    let r_ba = right.clone();
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: pair_svc.id.clone(),
        }],
        transport: wire_transport(
            provider.clone(),
            Arc::new(move || {
                for value in 1..=101 {
                    l_ba.change(BACKGROUND_CONTEXT.clone(), |draft| {
                        draft["value"] = json!(value);
                    })
                    .unwrap();
                    r_ba.change(BACKGROUND_CONTEXT.clone(), |draft| {
                        draft["value"] = json!(value);
                    })
                    .unwrap();
                }
            }),
            Arc::clone(&updates),
        ),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let pair = namespace.r#use::<ServiceObject>(&pair_svc).unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();

    let types: Vec<&str> = updates
        .lock()
        .unwrap()
        .iter()
        .map(|u| match u {
            ServiceProviderUpdate::Reset { .. } => "reset",
            ServiceProviderUpdate::State { .. } => "state",
            ServiceProviderUpdate::Unavailable => "unavailable",
            ServiceProviderUpdate::Replaced { .. } => "replaced",
            ServiceProviderUpdate::Spawned { .. } => "spawned",
            ServiceProviderUpdate::Closed { .. } => "closed",
        })
        .collect();
    assert_eq!(types, vec!["reset", "state"]);
    assert_eq!(get_state_value(&pair, "left").unwrap(), Some(json!({ "value": 101 })));
    assert_eq!(get_state_value(&pair, "right").unwrap(), Some(json!({ "value": 101 })));

    for value in 102..=104 {
        left.change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(value);
        })
        .unwrap();
        right
            .change(BACKGROUND_CONTEXT.clone(), |draft| {
                draft["value"] = json!(value);
            })
            .unwrap();
    }
    assert_eq!(get_state_value(&pair, "left").unwrap(), Some(json!({ "value": 104 })));
    assert_eq!(get_state_value(&pair, "right").unwrap(), Some(json!({ "value": 104 })));
    assert!(errors.lock().unwrap().is_empty());

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn an_overflow_reset_can_make_a_singleton_unavailable_before_a_later_replacement() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter.clone())]).unwrap();
    let s0 = replicated_state_initial(json!({ "value": 0 })).unwrap();
    provider.provide(&counter, counter_impl(&s0)).unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let prov_ba = provider.clone();
    let ctr_ba = counter.clone();
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: counter.id.clone() }],
        transport: wire_transport(
            provider.clone(),
            Arc::new(move || {
                for value in 1..=100 {
                    let s = replicated_state_initial(json!({ "value": value })).unwrap();
                    prov_ba.replace(&ctr_ba, counter_impl(&s)).unwrap();
                }
                prov_ba.withdraw(&ctr_ba).unwrap();
            }),
            Arc::clone(&updates),
        ),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let counter_handle = namespace.r#use::<ServiceObject>(&counter).unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        *updates.lock().unwrap(),
        vec![ServiceProviderUpdate::Reset {
            snapshot: ServiceSubscriptionSnapshot {
                service_id: counter.id.clone(),
                mode: ServiceMode::Singleton,
                instances: vec![],
            }
        }]
    );
    assert_eq!(get_state_value(&counter_handle, "state").unwrap(), None);

    let replacement = replicated_state_initial(json!({ "value": 200 })).unwrap();
    provider.replace(&counter, counter_impl(&replacement)).unwrap();
    replacement
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(201);
        })
        .unwrap();

    assert!(
        namespace
            .r#use::<ServiceObject>(&counter)
            .unwrap()
            .ptr_eq(&counter_handle)
    );
    assert_eq!(
        get_state_value(&counter_handle, "state").unwrap(),
        Some(json!({ "value": 201 }))
    );
    assert!(errors.lock().unwrap().is_empty());

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn keyed_resets_retain_live_generations_and_reconcile_closed_and_reused_keys() {
    let counter = svc_counter();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Definition {
        service: counter.clone(),
        mode: ServiceMode::Keyed,
    }])
    .unwrap();
    let retained = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let reused0 = replicated_state_initial(json!({ "value": 0 })).unwrap();
    let removed0 = replicated_state_initial(json!({ "value": 0 })).unwrap();

    provider.spawn(&counter, "retained", counter_impl(&retained)).unwrap();
    let close_old = provider.spawn(&counter, "reused", counter_impl(&reused0)).unwrap();
    let removed = provider.spawn(&counter, "removed", counter_impl(&removed0)).unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: counter.id.clone() }],
        transport: wire_transport(provider.clone(), Arc::new(|| {}), Arc::clone(&updates)),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let observed: Arc<Mutex<Vec<(ServiceObject, Context)>>> = Arc::new(Mutex::new(Vec::new()));
    let obs_cb = Arc::clone(&observed);
    let _stop = namespace
        .observe::<ServiceObject>(
            &counter,
            Arc::new(move |service, context| {
                obs_cb.lock().unwrap().push((service, context));
                Box::pin(futures::future::ready(Ok(())))
            }),
        )
        .unwrap();

    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(observed.lock().unwrap().len(), 3);

    let stable = observed.lock().unwrap()[1].clone();
    let old = observed.lock().unwrap()[2].clone();
    let replacement = replicated_state_initial(json!({ "value": 500 })).unwrap();

    let did_trigger = Arc::new(AtomicBool::new(false));
    let close_old_cb = Arc::new(Mutex::new(Some(close_old)));
    let removed_cb = Arc::new(Mutex::new(Some(removed)));
    let prov_cb = provider.clone();
    let ctr_cb = counter.clone();
    let repl_cb = replacement.clone();
    let ret_cb = retained.clone();
    let trig_cb = Arc::clone(&did_trigger);
    let _unsub = subscribe_state_values(&stable.0, "state", move |val| {
        if val["value"] != json!(1) || trig_cb.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(u) = close_old_cb.lock().unwrap().take() {
            u.call();
        }
        if let Some(u) = removed_cb.lock().unwrap().take() {
            u.call();
        }
        let _ = prov_cb.spawn(&ctr_cb, "reused", counter_impl(&repl_cb)).unwrap();
        ret_cb
            .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 102 }))
            .unwrap();
        for next in 501..=601 {
            repl_cb
                .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": next }))
                .unwrap();
        }
    })
    .unwrap();

    retained
        .replace(BACKGROUND_CONTEXT.clone(), json!({ "value": 1 }))
        .unwrap();
    assert!(
        updates
            .lock()
            .unwrap()
            .iter()
            .any(|u| matches!(u, ServiceProviderUpdate::Reset { .. }))
    );

    let obs = observed.lock().unwrap();
    assert_eq!(obs.len(), 4);
    assert!(!stable.1.abort_signal().unwrap().aborted());
    assert_eq!(
        get_state_value(&stable.0, "state").unwrap(),
        Some(json!({ "value": 102 }))
    );
    assert!(old.1.abort_signal().unwrap().aborted());
    assert!(obs[0].1.abort_signal().unwrap().aborted());
    assert_eq!(
        get_state_value(&obs[3].0, "state").unwrap(),
        Some(json!({ "value": 601 }))
    );
    drop(obs);

    replacement
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(602);
        })
        .unwrap();
    let obs = observed.lock().unwrap();
    assert_eq!(
        get_state_value(&obs[3].0, "state").unwrap(),
        Some(json!({ "value": 602 }))
    );
    drop(obs);
    assert!(errors.lock().unwrap().is_empty());

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

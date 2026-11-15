//! Port of packages/chord/test/services.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::{Op, OpReplace, OpSplice, ReplaceVerb, Seg, SpliceVerb};
use pi_chord::services::errors::{RemoteServiceError, RemoteServiceErrorCode};
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::provider::{RemoteServiceProviderEntry, RemoteServiceSubscription};
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{
    ReplicatedStateDeliveryKind, ReplicatedStateSourceSnapshot, ServiceIdRef, ServiceMemberMethodKind,
    ServiceMemberStateKind, ServiceMethodSnapshot, ServiceStateSnapshot,
};
use pi_chord::{
    Context, JsonValue, MutableReplicatedState, RemoteServiceBindingOptions, RemoteServiceProvider,
    RemoteServiceTransport, ReplicatedStateDelivery, ReplicatedStateSource, ReplicatedStateSourceAttachment,
    ReplicatedStateSourceFrame, Service, ServiceInstanceSnapshot, ServiceMemberSnapshot, ServiceMode,
    ServiceProviderUpdate, ServiceSubscription, ServiceSubscriptionSnapshot, create_remote_service_binding,
    define_service, replicated_state as replicated_state_initial, replicated_state_source,
};
use pi_js::abort::AbortSignal;
use serde_json::json;
use support::helpers::create_loopback_service_transport;

fn svc_models() -> Service {
    define_service("test.models", None).unwrap()
}

fn svc_question_dialogs() -> Service {
    define_service("test.question-dialog", None).unwrap()
}

fn svc_echo() -> Service {
    define_service("test.echo", None).unwrap()
}

fn svc_timeline() -> Service {
    define_service("test.timeline", None).unwrap()
}

struct MutableStateTarget(MutableReplicatedState<JsonValue>);

impl ServiceTarget for MutableStateTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let PropertyKey::String(key) = property else {
            return Ok(ServiceValue::Undefined);
        };
        if key == "value" {
            return Ok(ServiceValue::from_json(&self.0.value()));
        }
        Ok(ServiceValue::Undefined)
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

fn wrap_attached_state(state: &pi_chord::AttachedReplicatedState<JsonValue>) -> ServiceObject {
    let internals =
        get_replicated_state_internals(&state.as_replicated()).expect("attached state internals must be registered");
    struct AttachedTarget(pi_chord::AttachedReplicatedState<JsonValue>);
    impl ServiceTarget for AttachedTarget {
        fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
            match property {
                PropertyKey::String(k) if k == "value" => Ok(ServiceValue::from_json(&self.0.value())),
                _ => Ok(ServiceValue::Undefined),
            }
        }
        fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
            Err(pi_js::Error::js("TypeError", "AttachedReplicatedState is not callable"))
        }
    }
    ServiceObject::from_state(AttachedTarget(state.clone()), internals)
}

fn make_models_impl(
    state_obj: ServiceObject,
    on_select: impl Fn(JsonValue, Context) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync + 'static,
) -> Arc<ServiceObject> {
    let select_arc = Arc::new(on_select);
    let mut members = IndexMap::new();
    members.insert("state".to_string(), ServiceValue::Object(state_obj));
    members.insert(
        "select".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let model = args
                .first()
                .map(|v| v.to_json())
                .transpose()?
                .unwrap_or(JsonValue::Null);
            let ctx = args
                .get(1)
                .and_then(|v| match v {
                    ServiceValue::Object(o) => o.as_context(),
                    _ => None,
                })
                .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
            Ok(ServiceValue::Object(ServiceObject::from_promise((select_arc)(
                model, ctx,
            ))))
        })),
    );
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
        panic!("expected subscribe function");
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

async fn invoke_method(
    service: &ServiceObject,
    name: &str,
    args: Vec<ServiceValue>,
) -> pi_js::Result<Option<JsonValue>> {
    let ServiceValue::Object(method) = service.get_key(name)? else {
        panic!("expected method {name}");
    };
    let ServiceValue::Object(promise) = method.apply(None, &args)? else {
        panic!("expected promise from {name}");
    };
    promise.into_json_future().expect("method return must be promise").await
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

#[test]
fn checks_remote_json_contracts_only_at_compile_time() {
    // PORT: TS `@ts-expect-error` compile-time type checks are omitted in Rust;
    // runtime service flags (`local: false` default vs `local: true`) match TS.
    assert!(!define_service("test.json-passthrough", None).unwrap().local);
    assert!(
        define_service("test.local-non-json", Some(DefineServiceOptions { local: Some(true) }))
            .unwrap()
            .local
    );
}

#[test]
fn marks_services_remotable_by_default_and_reserves_chord_service_ids() {
    let models = svc_models();
    let local = define_service("test.local", Some(DefineServiceOptions { local: Some(true) })).unwrap();
    assert!(!models.local);
    assert!(local.local);

    let err_reserved = define_service("$chord.internal", Some(DefineServiceOptions { local: Some(true) })).unwrap_err();
    assert!(
        err_reserved
            .to_js_string()
            .contains("Service IDs beginning with $chord. are reserved")
    );

    let err_local = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(local)])
        .err()
        .expect("local service must reject on RemoteServiceProvider");
    assert!(err_local.to_js_string().contains("cannot be published remotely"));
}

#[test]
fn tracks_mutable_source_state_while_publishing_immutable_revisions() {
    let initial = json!({ "selected": null, "revision": 0 });
    let state = replicated_state_initial(initial.clone()).unwrap();
    let delivered: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let deliveries: Arc<Mutex<Vec<ReplicatedStateDeliveryKind>>> = Arc::new(Mutex::new(Vec::new()));

    let d_slot = Arc::clone(&delivered);
    let ds_slot = Arc::clone(&deliveries);
    let unsubscribe = state.subscribe(Arc::new(move |value, _ctx, delivery| {
        *d_slot.lock().unwrap() = Some(value);
        ds_slot.lock().unwrap().push(delivery.kind);
        Box::pin(futures::future::ready(Ok(())))
    }));

    assert_eq!(state.value(), initial);
    assert_eq!(delivered.lock().unwrap().as_ref(), Some(&state.value()));
    let hydrated = delivered.lock().unwrap().clone();

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["selected"] = json!({ "provider": "test", "modelId": "one" });
            draft["revision"] = json!(1);
        })
        .unwrap();

    assert_eq!(
        state.value(),
        json!({ "selected": { "provider": "test", "modelId": "one" }, "revision": 1 })
    );
    assert_ne!(state.value(), initial);
    assert_eq!(delivered.lock().unwrap().as_ref(), Some(&state.value()));
    assert_eq!(hydrated, Some(json!({ "selected": null, "revision": 0 })));
    assert_eq!(
        *deliveries.lock().unwrap(),
        vec![
            ReplicatedStateDeliveryKind::Hydrate,
            ReplicatedStateDeliveryKind::Update
        ]
    );
    unsubscribe.call();
}

#[test]
fn does_not_publish_when_a_transaction_restores_the_prior_value() {
    let state = replicated_state_initial(json!({ "value": 1 })).unwrap();
    let deliveries: Arc<Mutex<Vec<ReplicatedStateDelivery>>> = Arc::new(Mutex::new(Vec::new()));
    let d_slot = Arc::clone(&deliveries);
    let _unsub = state.subscribe(Arc::new(move |_value, _ctx, delivery| {
        d_slot.lock().unwrap().push(delivery);
        Box::pin(futures::future::ready(Ok(())))
    }));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(2);
            draft["value"] = json!(1);
        })
        .unwrap();

    assert_eq!(state.value(), json!({ "value": 1 }));
    assert_eq!(
        *deliveries.lock().unwrap(),
        vec![ReplicatedStateDelivery {
            kind: ReplicatedStateDeliveryKind::Hydrate,
            sequence: 0,
        }]
    );
}

#[test]
fn hydrates_a_new_subscriber_from_the_latest_atomic_revision() {
    let state = replicated_state_initial(json!({ "entries": [{ "id": "one" }] })).unwrap();
    let first: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let f_slot = Arc::clone(&first);
    let _u1 = state.subscribe(Arc::new(move |value, _ctx, _del| {
        f_slot.lock().unwrap().push(value);
        Box::pin(futures::future::ready(Ok(())))
    }));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["entries"].as_array_mut().unwrap().push(json!({ "id": "two" }));
        })
        .unwrap();

    let second: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let s_slot = Arc::clone(&second);
    let _u2 = state.subscribe(Arc::new(move |value, _ctx, _del| {
        s_slot.lock().unwrap().push(value);
        Box::pin(futures::future::ready(Ok(())))
    }));

    assert_eq!(
        *first.lock().unwrap(),
        vec![
            json!({ "entries": [{ "id": "one" }] }),
            json!({ "entries": [{ "id": "one" }, { "id": "two" }] }),
        ]
    );
    assert_eq!(
        *second.lock().unwrap(),
        vec![json!({ "entries": [{ "id": "one" }, { "id": "two" }] })]
    );
}

#[tokio::test]
async fn does_not_defensively_clone_method_arguments_or_results() {
    // PORT: Rust `JsonValue` args/results have value semantics across `ServiceCall` wire structs
    // rather than JS reference identity. Verify exact loopback value preservation without mutation.
    let echo_svc = svc_echo();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(echo_svc.clone())]).unwrap();
    let received: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));
    let response = json!({ "value": "response" });

    let rec = Arc::clone(&received);
    let resp = response.clone();
    let mut members = IndexMap::new();
    members.insert(
        "echo".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let payload = args
                .first()
                .map(|v| v.to_json())
                .transpose()?
                .unwrap_or(JsonValue::Null);
            *rec.lock().unwrap() = Some(payload);
            let r = resp.clone();
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(Some(r))),
            ))))
        })),
    );
    provider
        .provide(&echo_svc, Arc::new(ServiceObject::from_members(members)))
        .unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: echo_svc.id.clone(),
        }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let echo = namespace.r#use::<ServiceObject>(&echo_svc).unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();

    let request = json!({ "value": "request" });
    let result = invoke_method(
        &echo,
        "echo",
        vec![
            ServiceValue::from_json(&request),
            ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(result, Some(response));
    assert_eq!(*received.lock().unwrap(), Some(request));

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn provides_and_consumes_one_singleton_with_replicated_state() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    assert_eq!(
        provider.catalogue(),
        &[pi_chord::ServiceCatalogueEntry {
            service_id: models.id.clone(),
            mode: ServiceMode::Singleton,
        }]
    );

    let initial_state = json!({ "selected": null, "revision": 0 });
    let state = replicated_state_initial(initial_state.clone()).unwrap();
    let published_state: Arc<Mutex<Option<JsonValue>>> = Arc::new(Mutex::new(None));

    let st = state.clone();
    let pub_slot = Arc::clone(&published_state);
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), move |model, context| {
                let s = st.clone();
                let ps = Arc::clone(&pub_slot);
                Box::pin(async move {
                    s.change(context, |draft| {
                        draft["selected"] = model;
                        draft["revision"] = json!(draft["revision"].as_i64().unwrap_or(0) + 1);
                    })?;
                    *ps.lock().unwrap() = Some(s.value());
                    Ok(None)
                })
            }),
        )
        .unwrap();

    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let first = namespace.r#use::<ServiceObject>(&models).unwrap();
    let second = namespace.r#use::<ServiceObject>(&models).unwrap();
    assert!(first.ptr_eq(&second));
    assert_eq!(get_state_value(&first, "state").unwrap(), None);

    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(get_state_value(&first, "state").unwrap(), Some(initial_state));

    let updates: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let ups = Arc::clone(&updates);
    let unsub = subscribe_state_values(&second, "state", move |val| ups.lock().unwrap().push(val)).unwrap();

    invoke_method(
        &first,
        "select",
        vec![
            ServiceValue::from_json(&json!({ "provider": "test", "modelId": "one" })),
            ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
        ],
    )
    .await
    .unwrap();

    assert_eq!(
        get_state_value(&first, "state").unwrap(),
        *published_state.lock().unwrap()
    );
    assert_eq!(
        get_state_value(&first, "state").unwrap(),
        Some(json!({ "selected": { "provider": "test", "modelId": "one" }, "revision": 1 }))
    );
    assert_eq!(
        *updates.lock().unwrap(),
        vec![
            json!({ "selected": null, "revision": 0 }),
            json!({ "selected": { "provider": "test", "modelId": "one" }, "revision": 1 }),
        ]
    );
    assert!(errors.lock().unwrap().is_empty());

    let late_namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let late_models = late_namespace.r#use::<ServiceObject>(&models).unwrap();
    late_namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        get_state_value(&late_models, "state").unwrap().unwrap()["revision"],
        json!(1)
    );

    unsub.apply(None, &[]).unwrap();
    let _ = tokio::join!(
        namespace.dispose(BACKGROUND_CONTEXT.clone()),
        late_namespace.dispose(BACKGROUND_CONTEXT.clone())
    );
    provider.dispose().unwrap();
}

#[tokio::test]
async fn publishes_compact_tracked_operations_through_the_remote_provider() {
    let timeline_svc = svc_timeline();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(timeline_svc.clone())]).unwrap();
    let initial = json!({ "entries": [{ "id": "one" }], "retained": { "value": 1 } });
    let source = replicated_state_initial(initial.clone()).unwrap();

    let mut members = IndexMap::new();
    members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(&source)));
    provider
        .provide(&timeline_svc, Arc::new(ServiceObject::from_members(members)))
        .unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let ups_cb = Arc::clone(&updates);
    let raw = provider
        .subscribe(&timeline_svc.id, ServiceMode::Singleton, move |update, _ctx| {
            ups_cb.lock().unwrap().push(update);
        })
        .unwrap();
    assert_eq!(
        raw.snapshot().instances[0].members,
        vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
            name: "state".to_string(),
            kind: ServiceMemberStateKind::State,
            sequence: 0,
            ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, initial.clone()))],
        })]
    );
    raw.activate().unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: timeline_svc.id.clone(),
        }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let timeline = namespace.r#use::<ServiceObject>(&timeline_svc).unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    let previous = get_state_value(&timeline, "state").unwrap().unwrap();
    let next = json!({ "entries": [{ "id": "one" }, { "id": "two" }], "retained": { "value": 1 } });

    source
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["entries"].as_array_mut().unwrap().push(json!({ "id": "two" }));
        })
        .unwrap();

    let expected_first_op = ServiceProviderUpdate::State {
        instance: None,
        member: "state".to_string(),
        sequence: 1,
        ops: vec![Op::Splice(OpSplice(
            SpliceVerb::P,
            vec![Seg::String("entries".to_string())],
            1,
            0,
            vec![json!({ "id": "two" })],
        ))],
    };
    assert!(updates.lock().unwrap().contains(&expected_first_op));
    assert_eq!(previous, initial);
    assert_eq!(get_state_value(&timeline, "state").unwrap(), Some(next));

    source
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["entries"].as_array_mut().unwrap().push(json!({ "id": "three" }));
        })
        .unwrap();

    let late = provider
        .subscribe(&timeline_svc.id, ServiceMode::Singleton, |_, _| {})
        .unwrap();
    assert_eq!(
        updates.lock().unwrap().last(),
        Some(&ServiceProviderUpdate::State {
            instance: None,
            member: "state".to_string(),
            sequence: 2,
            ops: vec![Op::Splice(OpSplice(
                SpliceVerb::P,
                vec![Seg::String("entries".to_string())],
                2,
                0,
                vec![json!({ "id": "three" })],
            ))],
        })
    );
    assert_eq!(
        late.snapshot().instances[0].members,
        vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
            name: "state".to_string(),
            kind: ServiceMemberStateKind::State,
            sequence: 2,
            ops: vec![Op::Replace(OpReplace(
                ReplaceVerb::R,
                json!({
                    "entries": [{ "id": "one" }, { "id": "two" }, { "id": "three" }],
                    "retained": { "value": 1 }
                })
            ))],
        })]
    );

    late.close(None).unwrap();
    raw.close(None).unwrap();
    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[test]
fn publishes_authoritative_source_references_through_services_without_re_diffing() {
    let timeline_svc = svc_timeline();
    let initial = json!({ "entries": [{ "id": "one" }], "retained": { "value": 1 } });

    type Listener = Arc<dyn Fn(ReplicatedStateSourceFrame<JsonValue>) -> pi_js::Result<()> + Send + Sync>;
    struct SingleAttachment {
        snapshot: ReplicatedStateSourceSnapshot<JsonValue>,
        publish_slot: Arc<Mutex<Option<Listener>>>,
        disposed: Arc<AtomicBool>,
    }
    impl ReplicatedStateSourceAttachment<JsonValue> for SingleAttachment {
        fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> {
            &self.snapshot
        }
        fn activate(&self, listener: Listener) -> pi_js::Result<()> {
            *self.publish_slot.lock().unwrap() = Some(listener);
            Ok(())
        }
        fn dispose(&self) -> pi_js::Result<()> {
            self.disposed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    struct SingleSource {
        initial: JsonValue,
        publish_slot: Arc<Mutex<Option<Listener>>>,
        disposed: Arc<AtomicBool>,
    }
    impl ReplicatedStateSource<JsonValue> for SingleSource {
        fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue> + Send + Sync>> {
            Ok(Box::new(SingleAttachment {
                snapshot: ReplicatedStateSourceSnapshot {
                    value: self.initial.clone(),
                    cursor: 20,
                },
                publish_slot: Arc::clone(&self.publish_slot),
                disposed: Arc::clone(&self.disposed),
            }))
        }
    }

    let publish_slot: Arc<Mutex<Option<Listener>>> = Arc::new(Mutex::new(None));
    let disposed = Arc::new(AtomicBool::new(false));
    let state = replicated_state_source(
        Arc::new(SingleSource {
            initial: initial.clone(),
            publish_slot: Arc::clone(&publish_slot),
            disposed: Arc::clone(&disposed),
        }),
        None,
    )
    .unwrap();

    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(timeline_svc.clone())]).unwrap();
    let mut members = IndexMap::new();
    members.insert("state".to_string(), ServiceValue::Object(wrap_attached_state(&state)));
    provider
        .provide(&timeline_svc, Arc::new(ServiceObject::from_members(members)))
        .unwrap();

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let ups = Arc::clone(&updates);
    let subscription = provider
        .subscribe(&timeline_svc.id, ServiceMode::Singleton, move |update, _ctx| {
            ups.lock().unwrap().push(update);
        })
        .unwrap();
    let snapshot_member = &subscription.snapshot().instances[0].members[0];
    match snapshot_member {
        ServiceMemberSnapshot::State(s) => {
            assert_eq!(s.ops[0], Op::Replace(OpReplace(ReplaceVerb::R, initial.clone())))
        }
        _ => panic!("expected state member snapshot"),
    }
    subscription.activate().unwrap();

    let next = json!({ "entries": [{ "id": "one" }, { "id": "two" }], "retained": { "value": 1 } });
    let ops = vec![Op::Splice(OpSplice(
        SpliceVerb::P,
        vec![Seg::String("entries".to_string())],
        1,
        0,
        vec![json!({ "id": "two" })],
    ))];
    let publish = publish_slot.lock().unwrap().clone().unwrap();
    publish(ReplicatedStateSourceFrame {
        cursor: 21,
        value: next,
        ops: ops.clone(),
        context: BACKGROUND_CONTEXT.clone(),
    })
    .unwrap();

    let captured = updates.lock().unwrap();
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        ServiceProviderUpdate::State { ops: recv_ops, .. } => assert_eq!(*recv_ops, ops),
        other => panic!("expected state update, got {other:?}"),
    }
    drop(captured);

    subscription.close(None).unwrap();
    provider.dispose().unwrap();
    state.dispose().unwrap();
    assert!(disposed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn keeps_singleton_facades_stable_when_their_provider_is_replaced() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let initial_state = replicated_state_initial(json!({ "selected": null, "revision": 1 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&initial_state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let state_handle = match models_handle.get_key("state").unwrap() {
        ServiceValue::Object(o) => o,
        _ => panic!(),
    };
    let select_handle = match models_handle.get_key("select").unwrap() {
        ServiceValue::Object(o) => o,
        _ => panic!(),
    };

    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        state_handle.get_key("value").unwrap().to_json().unwrap()["revision"],
        json!(1)
    );

    provider.withdraw(&models).unwrap();
    assert!(matches!(
        state_handle.get_key("value").unwrap(),
        ServiceValue::Undefined
    ));

    let unavailable_promise = match select_handle
        .apply(
            None,
            &[
                ServiceValue::from_json(&json!({ "provider": "test", "modelId": "unavailable" })),
                ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
            ],
        )
        .unwrap()
    {
        ServiceValue::Object(p) => p,
        _ => panic!(),
    };
    let err_not_found = unavailable_promise.into_json_future().unwrap().await.unwrap_err();
    assert_eq!(
        err_not_found.downcast_ref::<RemoteServiceError>().map(|e| e.code),
        Some(RemoteServiceErrorCode::ServiceNotFound)
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cb = Arc::clone(&calls);
    let replacement_state = replicated_state_initial(json!({ "selected": null, "revision": 2 })).unwrap();
    provider
        .replace(
            &models,
            make_models_impl(wrap_mutable_state(&replacement_state), move |_, _| {
                calls_cb.fetch_add(1, Ordering::SeqCst);
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let re_used = namespace.r#use::<ServiceObject>(&models).unwrap();
    assert!(re_used.ptr_eq(&models_handle));
    let re_used_state = match models_handle.get_key("state").unwrap() {
        ServiceValue::Object(o) => o,
        _ => panic!(),
    };
    assert!(re_used_state.ptr_eq(&state_handle));
    assert_eq!(
        state_handle.get_key("value").unwrap().to_json().unwrap()["revision"],
        json!(2)
    );

    let ok_promise = match select_handle
        .apply(
            None,
            &[
                ServiceValue::from_json(&json!({ "provider": "test", "modelId": "replacement" })),
                ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
            ],
        )
        .unwrap()
    {
        ServiceValue::Object(p) => p,
        _ => panic!(),
    };
    ok_promise.into_json_future().unwrap().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let mut missing_state_members = IndexMap::new();
    missing_state_members.insert(
        "select".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(None)),
            ))))
        })),
    );
    let err_shape_missing = provider
        .replace(&models, Arc::new(ServiceObject::from_members(missing_state_members)))
        .unwrap_err();
    assert!(
        err_shape_missing
            .to_js_string()
            .contains("replacement must preserve its member shape")
    );

    let mut wrong_kind_members = IndexMap::new();
    wrong_kind_members.insert(
        "state".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(None)),
            ))))
        })),
    );
    wrong_kind_members.insert(
        "select".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(None)),
            ))))
        })),
    );
    let err_shape_kind = provider
        .replace(&models, Arc::new(ServiceObject::from_members(wrong_kind_members)))
        .unwrap_err();
    assert!(
        err_shape_kind
            .to_js_string()
            .contains("replacement must preserve its member shape")
    );
    assert_eq!(
        state_handle.get_key("value").unwrap().to_json().unwrap()["revision"],
        json!(2)
    );

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[test]
fn delivers_active_subscriber_updates_before_reporting_listener_failures() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let s1 = replicated_state_initial(json!({ "selected": null, "revision": 1 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&s1), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let delivered = Arc::new(AtomicUsize::new(0));
    let failing = provider
        .subscribe_fallible(&models.id, ServiceMode::Singleton, |_update, _ctx| {
            Err(pi_js::Error::js("Error", "listener failed"))
        })
        .unwrap();
    let del_cb = Arc::clone(&delivered);
    let succeeding = provider
        .subscribe(&models.id, ServiceMode::Singleton, move |_update, _ctx| {
            del_cb.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    failing.activate().unwrap();
    succeeding.activate().unwrap();

    let s2 = replicated_state_initial(json!({ "selected": null, "revision": 2 })).unwrap();
    let err = provider
        .replace(
            &models,
            make_models_impl(wrap_mutable_state(&s2), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap_err();
    assert!(err.to_js_string().contains("listener failed"));
    assert_eq!(delivered.load(Ordering::SeqCst), 1);

    failing.close(None).unwrap();
    succeeding.close(None).unwrap();
    provider.dispose().unwrap();
}

#[test]
fn does_not_replay_a_queued_revision_already_covered_by_a_new_subscription_snapshot() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let late_updates: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let late_holder: Arc<Mutex<Option<RemoteServiceSubscription>>> = Arc::new(Mutex::new(None));

    let st_cb = state.clone();
    let prov_cb = provider.clone();
    let lu_cb = Arc::clone(&late_updates);
    let lh_cb = Arc::clone(&late_holder);
    let first = provider
        .subscribe(&models.id.clone(), ServiceMode::Singleton, move |update, _ctx| {
            let ServiceProviderUpdate::State { sequence, .. } = update else {
                return;
            };
            if sequence != 1 {
                return;
            }
            st_cb
                .change(BACKGROUND_CONTEXT.clone(), |draft| {
                    draft["revision"] = json!(2);
                })
                .unwrap();
            let lu_inner = Arc::clone(&lu_cb);
            let late = prov_cb
                .subscribe(&models.id, ServiceMode::Singleton, move |next, _ctx| {
                    if let ServiceProviderUpdate::State { sequence, .. } = next {
                        lu_inner.lock().unwrap().push(sequence);
                    }
                })
                .unwrap();
            assert!(
                late.snapshot().instances[0]
                    .members
                    .contains(&ServiceMemberSnapshot::State(ServiceStateSnapshot {
                        name: "state".to_string(),
                        kind: ServiceMemberStateKind::State,
                        sequence: 2,
                        ops: vec![Op::Replace(OpReplace(
                            ReplaceVerb::R,
                            json!({ "selected": null, "revision": 2 })
                        ))],
                    }))
            );
            late.activate().unwrap();
            *lh_cb.lock().unwrap() = Some(late);
        })
        .unwrap();
    first.activate().unwrap();

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(1);
        })
        .unwrap();
    assert!(late_updates.lock().unwrap().is_empty());

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(3);
        })
        .unwrap();
    assert_eq!(*late_updates.lock().unwrap(), vec![3]);

    if let Some(late) = late_holder.lock().unwrap().take() {
        late.close(None).unwrap();
    }
    first.close(None).unwrap();
    provider.dispose().unwrap();
}

#[test]
fn replays_every_buffered_update_before_reporting_listener_failures() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let delivered = Arc::new(AtomicUsize::new(0));
    let del_cb = Arc::clone(&delivered);
    let subscription = provider
        .subscribe_fallible(&models.id, ServiceMode::Singleton, move |_update, _ctx| {
            del_cb.fetch_add(1, Ordering::SeqCst);
            Err(pi_js::Error::js("Error", "listener failed"))
        })
        .unwrap();

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(1);
        })
        .unwrap();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(2);
        })
        .unwrap();

    let err = subscription.activate().unwrap_err();
    assert!(
        err.to_js_string()
            .contains("Failed to activate remote service subscription")
    );
    assert_eq!(delivered.load(Ordering::SeqCst), 2);

    subscription.close(None).unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn clears_retained_facades_when_providers_and_bindings_are_disposed() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 1 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let state_handle = match models_handle.get_key("state").unwrap() {
        ServiceValue::Object(o) => o,
        _ => panic!(),
    };
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        state_handle.get_key("value").unwrap().to_json().unwrap()["revision"],
        json!(1)
    );

    provider.dispose().unwrap();
    assert!(matches!(
        state_handle.get_key("value").unwrap(),
        ServiceValue::Undefined
    ));
    let err_prov = invoke_method(
        &models_handle,
        "select",
        vec![
            ServiceValue::from_json(&json!({ "provider": "test", "modelId": "one" })),
            ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
        ],
    )
    .await
    .unwrap_err();
    assert!(err_prov.to_js_string().contains("Remote service provider is disposed"));

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    let err_bind = state_handle.get_key("value").err().expect("expected disposed error");
    assert!(err_bind.to_js_string().contains("Remote service binding is disposed"));
}

#[tokio::test]
async fn applies_provider_disposal_buffered_while_subscriptions_are_starting() {
    let models = svc_models();
    let dialogs = svc_question_dialogs();
    let provider = RemoteServiceProvider::new(vec![
        RemoteServiceProviderEntry::Service(models.clone()),
        RemoteServiceProviderEntry::Definition {
            service: dialogs.clone(),
            mode: ServiceMode::Keyed,
        },
    ])
    .unwrap();
    let models_state = replicated_state_initial(json!({ "selected": null, "revision": 1 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&models_state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let req_state = replicated_state_initial(json!({ "question": "Pending?" })).unwrap();
    let mut dialog_members = IndexMap::new();
    dialog_members.insert(
        "request".to_string(),
        ServiceValue::Object(wrap_mutable_state(&req_state)),
    );
    dialog_members.insert(
        "submit".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(Some(json!({ "accepted": true })))),
            ))))
        })),
    );
    provider
        .spawn(
            &dialogs,
            "pending",
            Arc::new(ServiceObject::from_members(dialog_members)),
        )
        .unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![
            ServiceIdRef { id: models.id.clone() },
            ServiceIdRef { id: dialogs.id.clone() },
        ],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let observed: Arc<Mutex<Vec<ServiceObject>>> = Arc::new(Mutex::new(Vec::new()));
    let obs_cb = Arc::clone(&observed);
    let _stop = namespace
        .observe::<ServiceObject>(
            &dialogs,
            Arc::new(move |svc, _ctx| {
                obs_cb.lock().unwrap().push(svc);
                Box::pin(futures::future::ready(Ok(())))
            }),
        )
        .unwrap();

    provider.dispose().unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(get_state_value(&models_handle, "state").unwrap(), None);
    assert!(observed.lock().unwrap().is_empty());

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
}

#[tokio::test]
async fn keeps_deferred_service_handles_inaccessible_until_host_activation() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let active = Arc::new(AtomicBool::new(false));
    let active_chk = Arc::clone(&active);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: Some(false),
        on_error: None,
        assert_access: Some(Arc::new(move || {
            if !active_chk.load(Ordering::SeqCst) {
                return Err(pi_js::Error::js("Error", "Service handles are not active"));
            }
            Ok(())
        })),
    })
    .unwrap();

    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let ServiceValue::Object(state_obj) = models_handle.get_key("state").unwrap() else {
        panic!();
    };
    let err_val = state_obj.get_key("value").err().expect("expected inactive error");
    assert!(err_val.to_js_string().contains("Service handles are not active"));

    let ServiceValue::Object(sub_fn) = state_obj.get_key("subscribe").unwrap() else {
        panic!();
    };
    let err_sub = sub_fn
        .apply(
            None,
            &[ServiceValue::Object(ServiceObject::from_fn(|_, _| {
                Ok(ServiceValue::Undefined)
            }))],
        )
        .err()
        .expect("expected inactive error");
    assert!(err_sub.to_js_string().contains("Service handles are not active"));

    let ServiceValue::Object(sel_fn) = models_handle.get_key("select").unwrap() else {
        panic!();
    };
    let err_call = sel_fn
        .apply(
            None,
            &[
                ServiceValue::from_json(&json!({ "provider": "test", "modelId": "one" })),
                ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
            ],
        )
        .err()
        .expect("expected inactive error");
    assert!(err_call.to_js_string().contains("Service handles are not active"));

    namespace.rebind(true, BACKGROUND_CONTEXT.clone()).await.unwrap();
    active.store(true, Ordering::SeqCst);
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        state_obj.get_key("value").unwrap().to_json().unwrap()["revision"],
        json!(0)
    );

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn rejects_namespace_readiness_when_initial_hydration_fails() {
    let models = svc_models();
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: RemoteServiceTransport::new(
            Arc::new(|_call, _ctx| {
                Box::pin(futures::future::ready(Err(pi_js::Error::js(
                    "Error",
                    "unexpected invocation",
                ))))
            }),
            Arc::new(|_svc_id, _mode, _listener, _ctx| {
                Box::pin(futures::future::ready(Err(pi_js::Error::js(
                    "Error",
                    "initial hydration failed",
                ))))
            }),
        ),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let ready_err = namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap_err();
    assert!(ready_err.to_js_string().contains("initial hydration failed"));
    assert_eq!(get_state_value(&models_handle, "state").unwrap(), None);
    let captured = errors.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].to_js_string().contains("initial hydration failed"));
    drop(captured);

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
}

#[tokio::test]
async fn buffers_state_updates_that_race_subscription_hydration() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let prov_t = provider.clone();
    let st_t = state.clone();
    let transport = RemoteServiceTransport::new(
        Arc::new(move |call, ctx| {
            let p = prov_t.clone();
            let c = call.clone();
            let x = ctx.clone();
            Box::pin(async move { p.invoke(&c, x).await })
        }),
        Arc::new(move |svc_id, mode, listener, _ctx| {
            let p = provider.clone();
            let s = st_t.clone();
            let id = svc_id.to_string();
            Box::pin(async move {
                let subscription = p.subscribe(&id, mode, move |upd, ctx| listener(upd, ctx))?;
                s.change(BACKGROUND_CONTEXT.clone(), |draft| {
                    draft["revision"] = json!(1);
                })?;
                let snapshot = subscription.snapshot().clone();
                let sub_act = subscription.clone();
                let sub_cls = subscription.clone();
                Ok(ServiceSubscription::new(
                    snapshot,
                    Arc::new(move || sub_act.activate()),
                    Arc::new(move |ctx| {
                        let s_c = sub_cls.clone();
                        Box::pin(async move { s_c.close(ctx) })
                    }),
                ))
            })
        }),
    );

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport,
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let revisions: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let revs = Arc::clone(&revisions);
    let _unsub = subscribe_state_values(&models_handle, "state", move |val| {
        if let Some(r) = val["revision"].as_i64() {
            revs.lock().unwrap().push(r);
        }
    })
    .unwrap();

    wait_for(|| *revisions.lock().unwrap() == vec![0, 1]).await;
    assert_eq!(
        get_state_value(&models_handle, "state").unwrap().unwrap()["revision"],
        json!(1)
    );
    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
}

async fn run_sequence_error_case(sequence: i64) {
    let models = svc_models();
    type UpdateSender = Arc<dyn Fn(ServiceProviderUpdate, Context) + Send + Sync>;
    let send_update: Arc<Mutex<Option<UpdateSender>>> = Arc::new(Mutex::new(None));
    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));

    let su_cb = Arc::clone(&send_update);
    let models_id = models.id.clone();
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: RemoteServiceTransport::new(
            Arc::new(|_, _| {
                Box::pin(futures::future::ready(Err(pi_js::Error::js(
                    "Error",
                    "unexpected invocation",
                ))))
            }),
            Arc::new(move |_svc_id, _mode, listener, _ctx| {
                let su_cb = Arc::clone(&su_cb);
                let models_id = models_id.clone();
                Box::pin(async move {
                    *su_cb.lock().unwrap() = Some(listener);
                    let snapshot = ServiceSubscriptionSnapshot {
                        service_id: models_id,
                        mode: ServiceMode::Singleton,
                        instances: vec![ServiceInstanceSnapshot {
                            instance: None,
                            members: vec![
                                ServiceMemberSnapshot::Method(ServiceMethodSnapshot {
                                    name: "select".to_string(),
                                    kind: ServiceMemberMethodKind::Method,
                                }),
                                ServiceMemberSnapshot::State(ServiceStateSnapshot {
                                    name: "state".to_string(),
                                    kind: ServiceMemberStateKind::State,
                                    sequence: 0,
                                    ops: vec![Op::Replace(OpReplace(
                                        ReplaceVerb::R,
                                        json!({ "selected": null, "revision": 0 }),
                                    ))],
                                }),
                            ],
                        }],
                    };
                    Ok(ServiceSubscription::new(
                        snapshot,
                        Arc::new(|| Ok(())),
                        Arc::new(|_| Box::pin(futures::future::ready(Ok(())))),
                    ))
                })
            }),
        ),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    namespace.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        get_state_value(&models_handle, "state").unwrap().unwrap()["revision"],
        json!(0)
    );

    let sender = send_update.lock().unwrap().clone().unwrap();
    sender(
        ServiceProviderUpdate::State {
            instance: None,
            member: "state".to_string(),
            sequence,
            ops: vec![Op::Replace(OpReplace(
                ReplaceVerb::R,
                json!({ "selected": null, "revision": sequence }),
            ))],
        },
        BACKGROUND_CONTEXT.clone(),
    );

    assert_eq!(get_state_value(&models_handle, "state").unwrap(), None);
    let errs = errors.lock().unwrap();
    assert_eq!(errs.len(), 1);
    assert!(
        errs[0].to_js_string().contains("sequence has a gap"),
        "unexpected error: {}",
        errs[0].to_js_string()
    );
    drop(errs);

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
}

#[tokio::test]
async fn clears_replicated_state_after_a_duplicate_operation_sequence() {
    run_sequence_error_case(0).await;
}

#[tokio::test]
async fn clears_replicated_state_after_a_gap_operation_sequence() {
    run_sequence_error_case(2).await;
}

#[tokio::test]
async fn hydrates_cold_replicated_state_replicas_and_replaces_them_across_rebinds() {
    let models = svc_models();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(models.clone())]).unwrap();
    let state = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&state), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: models.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: Some(false),
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let models_handle = namespace.r#use::<ServiceObject>(&models).unwrap();
    let revisions: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let revs = Arc::clone(&revisions);
    let _unsub = subscribe_state_values(&models_handle, "state", move |val| {
        if let Some(r) = val["revision"].as_i64() {
            revs.lock().unwrap().push(r);
        }
    })
    .unwrap();

    assert_eq!(get_state_value(&models_handle, "state").unwrap(), None);
    assert!(revisions.lock().unwrap().is_empty());

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(1);
        })
        .unwrap();
    namespace.rebind(true, BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        get_state_value(&models_handle, "state").unwrap().unwrap()["revision"],
        json!(1)
    );
    assert_eq!(*revisions.lock().unwrap(), vec![1]);

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(2);
        })
        .unwrap();
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2]);

    namespace.rebind(false, BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(get_state_value(&models_handle, "state").unwrap(), None);
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["revision"] = json!(3);
        })
        .unwrap();
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2]);

    namespace.rebind(true, BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(
        get_state_value(&models_handle, "state").unwrap().unwrap()["revision"],
        json!(3)
    );
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2, 3]);

    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[tokio::test]
async fn hydrates_keyed_state_before_observe_handlers_and_fences_reused_keys() {
    let dialogs = svc_question_dialogs();
    let provider = RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Definition {
        service: dialogs.clone(),
        mode: ServiceMode::Keyed,
    }])
    .unwrap();
    assert_eq!(
        provider.catalogue(),
        &[pi_chord::ServiceCatalogueEntry {
            service_id: dialogs.id.clone(),
            mode: ServiceMode::Keyed,
        }]
    );

    let errors: Arc<Mutex<Vec<pi_js::Error>>> = Arc::new(Mutex::new(Vec::new()));
    let errs_cb = Arc::clone(&errors);
    let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef { id: dialogs.id.clone() }],
        transport: create_loopback_service_transport(&provider),
        bound: None,
        on_error: Some(Arc::new(move |err| errs_cb.lock().unwrap().push(err))),
        assert_access: None,
    })
    .unwrap();

    struct Observed {
        question: Option<JsonValue>,
        service: ServiceObject,
        context: Context,
    }
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let obs_cb = Arc::clone(&observed);
    let stop = namespace
        .observe::<ServiceObject>(
            &dialogs,
            Arc::new(move |service, context| {
                let q = get_state_value(&service, "request").unwrap();
                obs_cb.lock().unwrap().push(Observed {
                    question: q,
                    service,
                    context,
                });
                Box::pin(futures::future::ready(Ok(())))
            }),
        )
        .unwrap();

    wait_for(|| errors.lock().unwrap().is_empty()).await;

    let first_request = replicated_state_initial(json!({ "question": "First?" })).unwrap();
    let submit_calls: Arc<Mutex<Vec<(JsonValue, Option<AbortSignal>)>>> = Arc::new(Mutex::new(Vec::new()));
    let sc_cb = Arc::clone(&submit_calls);
    let mut first_members = IndexMap::new();
    first_members.insert(
        "request".to_string(),
        ServiceValue::Object(wrap_mutable_state(&first_request)),
    );
    first_members.insert(
        "submit".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let answer = args
                .first()
                .map(|v| v.to_json())
                .transpose()?
                .unwrap_or(JsonValue::Null);
            let ctx = args
                .get(1)
                .and_then(|v| match v {
                    ServiceValue::Object(o) => o.as_context(),
                    _ => None,
                })
                .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
            sc_cb.lock().unwrap().push((answer, ctx.abort_signal()));
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(Some(json!({ "accepted": true })))),
            ))))
        })),
    );
    let close_first = provider
        .spawn(
            &dialogs,
            "invocation-1",
            Arc::new(ServiceObject::from_members(first_members)),
        )
        .unwrap();

    wait_for(|| observed.lock().unwrap().len() == 1).await;
    let first_service = {
        let obs = observed.lock().unwrap();
        assert_eq!(obs[0].question, Some(json!({ "question": "First?" })));
        obs[0].service.clone()
    };

    first_request
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["question"] = json!("Updated?");
        })
        .unwrap();
    assert_eq!(
        get_state_value(&first_service, "request").unwrap(),
        Some(json!({ "question": "Updated?" }))
    );

    let res = invoke_method(
        &first_service,
        "submit",
        vec![
            ServiceValue::String("yes".to_string()),
            ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(res, Some(json!({ "accepted": true })));
    {
        let calls = submit_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, json!("yes"));
        assert!(calls[0].1.is_none());
    }

    let ServiceValue::Object(retained_first_submit) = first_service.get_key("submit").unwrap() else {
        panic!();
    };
    close_first.call();

    assert!(observed.lock().unwrap()[0].context.abort_signal().unwrap().aborted());
    let err_closed_state = get_state_value(&first_service, "request").unwrap_err();
    assert!(err_closed_state.to_js_string().contains("observation is closed"));
    let err_closed_call = retained_first_submit
        .apply(
            None,
            &[
                ServiceValue::String("late".to_string()),
                ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
            ],
        )
        .err()
        .expect("expected closed error");
    assert!(err_closed_call.to_js_string().contains("observation is closed"));

    let second_request = replicated_state_initial(json!({ "question": "Again?" })).unwrap();
    let mut second_members = IndexMap::new();
    second_members.insert(
        "request".to_string(),
        ServiceValue::Object(wrap_mutable_state(&second_request)),
    );
    second_members.insert(
        "submit".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(Some(json!({ "accepted": false })))),
            ))))
        })),
    );
    let close_second = provider
        .spawn(
            &dialogs,
            "invocation-1",
            Arc::new(ServiceObject::from_members(second_members)),
        )
        .unwrap();

    wait_for(|| observed.lock().unwrap().len() == 2).await;
    let second_service = {
        let obs = observed.lock().unwrap();
        assert_eq!(obs[1].question, Some(json!({ "question": "Again?" })));
        assert!(!obs[1].service.ptr_eq(&first_service));
        obs[1].service.clone()
    };
    assert!(errors.lock().unwrap().is_empty());

    let ServiceValue::Object(retained_second_submit) = second_service.get_key("submit").unwrap() else {
        panic!();
    };
    stop.call();
    assert!(observed.lock().unwrap()[1].context.abort_signal().unwrap().aborted());
    let err_stop_state = get_state_value(&second_service, "request").unwrap_err();
    assert!(err_stop_state.to_js_string().contains("observation is closed"));
    let err_stop_call = retained_second_submit
        .apply(
            None,
            &[
                ServiceValue::String("late".to_string()),
                ServiceValue::Object(ServiceObject::from_context(BACKGROUND_CONTEXT.clone())),
            ],
        )
        .err()
        .expect("expected closed error");
    assert!(err_stop_call.to_js_string().contains("observation is closed"));

    close_second.call();
    namespace.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    provider.dispose().unwrap();
}

#[test]
fn rejects_mode_mixing_and_unsupported_members() {
    let models = svc_models();
    let dialogs = svc_question_dialogs();
    let provider = RemoteServiceProvider::new(vec![
        RemoteServiceProviderEntry::Service(models.clone()),
        RemoteServiceProviderEntry::Definition {
            service: dialogs.clone(),
            mode: ServiceMode::Keyed,
        },
    ])
    .unwrap();
    let s = replicated_state_initial(json!({ "selected": null, "revision": 0 })).unwrap();
    provider
        .provide(
            &models,
            make_models_impl(wrap_mutable_state(&s), |_, _| {
                Box::pin(futures::future::ready(Ok(None)))
            }),
        )
        .unwrap();

    let err_singleton = provider
        .spawn(&models, "wrong", Arc::new(ServiceObject::from_members(IndexMap::new())))
        .unwrap_err();
    assert!(err_singleton.to_js_string().contains("singleton"));

    let mut bad_members = IndexMap::new();
    bad_members.insert(
        "request".to_string(),
        ServiceValue::Object(ServiceObject::from_json(json!({ "not": "state" }))),
    );
    bad_members.insert(
        "submit".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(|_, _| {
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                futures::future::ready(Ok(Some(json!({ "accepted": true })))),
            ))))
        })),
    );
    let err_member = provider
        .spawn(&dialogs, "invalid", Arc::new(ServiceObject::from_members(bad_members)))
        .unwrap_err();
    assert!(err_member.to_js_string().contains("not remotely exposable"));

    provider.dispose().unwrap();
}

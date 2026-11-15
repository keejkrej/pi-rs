//! Port of packages/chord/test/service-wire.test.ts

use std::sync::{Arc, Mutex};

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::{
    DefineVerb, Op, OpReplace, OpSet, PathRef, ReplaceVerb, Seg, SetVerb, WireOp, WireOpDefine, WireOpSet,
};
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::provider::RemoteServiceProviderEntry;
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::services::wire::{
    ServiceControlCall, WireServiceInstanceSnapshot, WireServiceMemberSnapshot, WireServiceProviderUpdate,
    WireServiceStateKind, WireServiceStateSnapshot, WireServiceSubscriptionSnapshot, create_service_catalogue_call,
    create_service_subscribe_call, create_service_unsubscribe_call, decode_service_control_call, parse_service_call,
    parse_service_catalogue, parse_service_provider_update, parse_service_subscription_snapshot,
    parse_wire_service_provider_update, parse_wire_service_subscription_snapshot,
};
use pi_chord::types::{ServiceMemberStateKind, ServiceStateSnapshot};
use pi_chord::{
    JsonValue, MutableReplicatedState, RemoteServiceProvider, ServiceCall, ServiceCatalogueEntry,
    ServiceInstanceAddress, ServiceInstanceSnapshot, ServiceMemberSnapshot, ServiceMode, ServiceProviderUpdate,
    ServiceSubscriptionSnapshot, create_remote_service_endpoint, create_service_state_decoder,
    create_service_state_encoder, define_service, replicated_state,
};
use serde_json::json;

#[test]
fn encodes_control_calls_and_validates_service_values() {
    assert_eq!(
        decode_service_control_call(&create_service_catalogue_call()),
        Some(ServiceControlCall::Catalogue)
    );
    assert_eq!(
        parse_service_catalogue(&json!([
            { "serviceId": "pi.models", "mode": "singleton" },
            { "serviceId": "pi.dialogs", "mode": "keyed" }
        ]))
        .unwrap(),
        vec![
            ServiceCatalogueEntry {
                service_id: "pi.models".to_string(),
                mode: ServiceMode::Singleton,
            },
            ServiceCatalogueEntry {
                service_id: "pi.dialogs".to_string(),
                mode: ServiceMode::Keyed,
            },
        ]
    );
    let subscribe = create_service_subscribe_call("subscription-1", "pi.models", ServiceMode::Singleton);
    assert_eq!(
        decode_service_control_call(&subscribe),
        Some(ServiceControlCall::Subscribe {
            subscription_id: "subscription-1".to_string(),
            service_id: "pi.models".to_string(),
            mode: ServiceMode::Singleton,
        })
    );
    assert_eq!(
        decode_service_control_call(&create_service_unsubscribe_call("subscription-1")),
        Some(ServiceControlCall::Unsubscribe {
            subscription_id: "subscription-1".to_string(),
        })
    );
    let call = parse_service_call(&json!({
        "serviceId": "pi.question-dialog",
        "instance": { "key": "invocation-1", "generation": 2 },
        "member": "submit",
        "args": [{ "outcome": "selected", "index": 0 }],
    }))
    .unwrap();
    assert_eq!(call.member, "submit");
}

#[test]
fn rejects_malformed_service_values() {
    let err1 = parse_service_call(&json!({
        "serviceId": "pi.models",
        "member": "list",
        "args": [],
        "extra": true,
    }))
    .unwrap_err();
    assert!(err1.message().contains("Invalid service call"), "got: {err1}");

    let err2 = parse_service_catalogue(&json!([{ "serviceId": "pi.models", "mode": "unknown" }])).unwrap_err();
    assert!(err2.message().contains("Invalid service catalogue"), "got: {err2}");

    let err3 = parse_service_provider_update(&json!({
        "type": "state",
        "member": "state",
        "sequence": 0,
        "ops": [],
    }))
    .unwrap_err();
    assert!(err3.message().contains("Invalid service state update"), "got: {err3}");

    assert!(
        parse_wire_service_provider_update(&json!({
            "type": "state",
            "member": "state",
            "sequence": 1,
            "ops": [["?", 0]],
        }))
        .is_err()
    );
}

#[test]
fn validates_decoded_and_wire_snapshots_and_updates() {
    let snapshot = ServiceSubscriptionSnapshot {
        service_id: "pi.models".to_string(),
        mode: ServiceMode::Singleton,
        instances: vec![ServiceInstanceSnapshot {
            instance: None,
            members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                name: "state".to_string(),
                kind: ServiceMemberStateKind::State,
                sequence: 0,
                ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "revision": 1 })))],
            })],
        }],
    };
    let parsed_snap = parse_service_subscription_snapshot(&serde_json::to_value(&snapshot).unwrap()).unwrap();
    assert_eq!(parsed_snap, snapshot);

    let encoder = create_service_state_encoder();
    let wire_snapshot = encoder.encode_snapshot(&snapshot).unwrap();
    let parsed_wire_snap =
        parse_wire_service_subscription_snapshot(&serde_json::to_value(&wire_snapshot).unwrap()).unwrap();
    assert_eq!(parsed_wire_snap, wire_snapshot);

    let update = ServiceProviderUpdate::State {
        instance: None,
        member: "state".to_string(),
        sequence: 1,
        ops: vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("revision".into())],
            json!(2),
        ))],
    };
    let parsed_update = parse_service_provider_update(&serde_json::to_value(&update).unwrap()).unwrap();
    assert_eq!(parsed_update, update);

    let wire_update = encoder.encode_update(&update).unwrap();
    let parsed_wire_update = parse_wire_service_provider_update(&serde_json::to_value(&wire_update).unwrap()).unwrap();
    assert_eq!(parsed_wire_update, wire_update);
}

#[test]
fn keeps_one_operation_codec_pair_for_one_subscription_state() {
    let enc = create_service_state_encoder();
    let dec = create_service_state_decoder();
    let snapshot = ServiceSubscriptionSnapshot {
        service_id: "pi.models".to_string(),
        mode: ServiceMode::Singleton,
        instances: vec![ServiceInstanceSnapshot {
            instance: None,
            members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                name: "state".to_string(),
                kind: ServiceMemberStateKind::State,
                sequence: 0,
                ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "revision": 0 })))],
            })],
        }],
    };
    assert_eq!(
        dec.decode_snapshot(&enc.encode_snapshot(&snapshot).unwrap()).unwrap(),
        snapshot
    );

    let first = ServiceProviderUpdate::State {
        instance: None,
        member: "state".to_string(),
        sequence: 1,
        ops: vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("revision".into())],
            json!(1),
        ))],
    };
    let second = ServiceProviderUpdate::State {
        instance: None,
        member: "state".to_string(),
        sequence: 2,
        ops: vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("revision".into())],
            json!(2),
        ))],
    };
    let first_wire = enc.encode_update(&first).unwrap();
    let second_wire = enc.encode_update(&second).unwrap();
    assert_eq!(
        serde_json::to_value(&first_wire).unwrap()["ops"],
        json!([["s", ["revision"], 1]])
    );
    assert_eq!(
        second_wire,
        WireServiceProviderUpdate::State {
            instance: None,
            member: "state".to_string(),
            sequence: 2,
            ops: vec![
                WireOp::Define(WireOpDefine(
                    DefineVerb::Define,
                    0,
                    vec![Seg::String("revision".into())]
                )),
                WireOp::Set(WireOpSet(SetVerb::S, PathRef::Id(0), json!(2))),
            ],
        }
    );
    assert_eq!(dec.decode_update(&first_wire).unwrap(), first);
    assert_eq!(dec.decode_update(&second_wire).unwrap(), second);
}

#[test]
fn validates_explicit_resets_and_restarts_path_dictionaries_at_the_new_baseline() {
    let enc = create_service_state_encoder();
    let dec = create_service_state_decoder();
    let snapshot = ServiceSubscriptionSnapshot {
        service_id: "pi.states".to_string(),
        mode: ServiceMode::Singleton,
        instances: vec![ServiceInstanceSnapshot {
            instance: None,
            members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                name: "state".to_string(),
                kind: ServiceMemberStateKind::State,
                sequence: 0,
                ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "before": 0 })))],
            })],
        }],
    };
    dec.decode_snapshot(&enc.encode_snapshot(&snapshot).unwrap()).unwrap();
    for sequence in 1..=2 {
        dec.decode_update(
            &enc.encode_update(&ServiceProviderUpdate::State {
                instance: None,
                member: "state".to_string(),
                sequence,
                ops: vec![Op::Set(OpSet(
                    SetVerb::S,
                    vec![Seg::String("before".into())],
                    json!(sequence),
                ))],
            })
            .unwrap(),
        )
        .unwrap();
    }
    let reset = ServiceProviderUpdate::Reset {
        snapshot: ServiceSubscriptionSnapshot {
            service_id: snapshot.service_id.clone(),
            mode: snapshot.mode,
            instances: vec![ServiceInstanceSnapshot {
                instance: None,
                members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                    name: "state".to_string(),
                    kind: ServiceMemberStateKind::State,
                    sequence: 103,
                    ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "after": 103 })))],
                })],
            }],
        },
    };
    let parsed_reset = parse_service_provider_update(&serde_json::to_value(&reset).unwrap()).unwrap();
    assert_eq!(parsed_reset, reset);
    let wire_reset = enc.encode_update(&reset).unwrap();
    let parsed_wire_reset = parse_wire_service_provider_update(&serde_json::to_value(&wire_reset).unwrap()).unwrap();
    assert_eq!(parsed_wire_reset, wire_reset);
    assert_eq!(dec.decode_update(&wire_reset).unwrap(), reset);

    for sequence in 104..=106 {
        let update = ServiceProviderUpdate::State {
            instance: None,
            member: "state".to_string(),
            sequence,
            ops: vec![Op::Set(OpSet(
                SetVerb::S,
                vec![Seg::String("after".into())],
                json!(sequence),
            ))],
        };
        assert_eq!(dec.decode_update(&enc.encode_update(&update).unwrap()).unwrap(), update);
    }

    let reset_json = serde_json::to_value(&reset).unwrap();
    let mut with_extra = reset_json.as_object().unwrap().clone();
    with_extra.insert("extra".to_string(), json!(true));
    let extra_val = serde_json::Value::Object(with_extra);

    let invalid_ops_reset = json!({
        "type": "reset",
        "snapshot": {
            "serviceId": snapshot.service_id,
            "mode": snapshot.mode,
            "instances": [
                {
                    "members": [{ "name": "state", "kind": "state", "sequence": 103, "ops": [["s", ["before"], 103]] }]
                }
            ]
        }
    });

    assert!(parse_service_provider_update(&extra_val).is_err());
    assert!(parse_wire_service_provider_update(&extra_val).is_err());
    assert!(parse_service_provider_update(&json!({ "type": "reset", "snapshot": {} })).is_err());
    assert!(parse_wire_service_provider_update(&json!({ "type": "reset", "snapshot": {} })).is_err());
    let err1 = parse_service_provider_update(&invalid_ops_reset).unwrap_err();
    assert!(err1.message().contains("full root replacements"), "got: {err1}");
    let err2 = parse_wire_service_provider_update(&invalid_ops_reset).unwrap_err();
    assert!(err2.message().contains("full root replacements"), "got: {err2}");
}

#[test]
fn isolates_operation_dictionaries_between_states_and_subscriptions() {
    let snapshot = ServiceSubscriptionSnapshot {
        service_id: "pi.states".to_string(),
        mode: ServiceMode::Singleton,
        instances: vec![ServiceInstanceSnapshot {
            instance: None,
            members: vec![
                ServiceMemberSnapshot::State(ServiceStateSnapshot {
                    name: "left".to_string(),
                    kind: ServiceMemberStateKind::State,
                    sequence: 0,
                    ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "revision": 0 })))],
                }),
                ServiceMemberSnapshot::State(ServiceStateSnapshot {
                    name: "right".to_string(),
                    kind: ServiceMemberStateKind::State,
                    sequence: 0,
                    ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "revision": 0 })))],
                }),
            ],
        }],
    };
    let first_encoder = create_service_state_encoder();
    let first_decoder = create_service_state_decoder();
    let second_encoder = create_service_state_encoder();
    let second_decoder = create_service_state_decoder();
    first_decoder
        .decode_snapshot(&first_encoder.encode_snapshot(&snapshot).unwrap())
        .unwrap();
    second_decoder
        .decode_snapshot(&second_encoder.encode_snapshot(&snapshot).unwrap())
        .unwrap();

    let make_update = |member: &str, sequence: i64, revision: i64| ServiceProviderUpdate::State {
        instance: None,
        member: member.to_string(),
        sequence,
        ops: vec![Op::Set(OpSet(
            SetVerb::S,
            vec![Seg::String("revision".into())],
            json!(revision),
        ))],
    };
    let first_left = first_encoder.encode_update(&make_update("left", 1, 1)).unwrap();
    let first_right = first_encoder.encode_update(&make_update("right", 1, 1)).unwrap();
    let second_left = first_encoder.encode_update(&make_update("left", 2, 2)).unwrap();
    let second_right = first_encoder.encode_update(&make_update("right", 2, 2)).unwrap();

    assert_eq!(
        serde_json::to_value(&first_left).unwrap()["ops"],
        json!([["s", ["revision"], 1]])
    );
    assert_eq!(
        serde_json::to_value(&first_right).unwrap()["ops"],
        json!([["s", ["revision"], 1]])
    );
    assert_eq!(
        serde_json::to_value(&second_left).unwrap()["ops"],
        json!([["#", 0, ["revision"]], ["s", 0, 2]])
    );
    assert_eq!(
        serde_json::to_value(&second_right).unwrap()["ops"],
        json!([["#", 0, ["revision"]], ["s", 0, 2]])
    );
    assert_eq!(
        first_decoder.decode_update(&first_left).unwrap(),
        make_update("left", 1, 1)
    );
    assert_eq!(
        first_decoder.decode_update(&first_right).unwrap(),
        make_update("right", 1, 1)
    );
    assert_eq!(
        first_decoder.decode_update(&second_left).unwrap(),
        make_update("left", 2, 2)
    );
    assert_eq!(
        first_decoder.decode_update(&second_right).unwrap(),
        make_update("right", 2, 2)
    );

    let independent_left = second_encoder.encode_update(&make_update("left", 1, 1)).unwrap();
    assert_eq!(
        serde_json::to_value(&independent_left).unwrap()["ops"],
        json!([["s", ["revision"], 1]])
    );
    assert_eq!(
        second_decoder.decode_update(&independent_left).unwrap(),
        make_update("left", 1, 1)
    );

    let left_base = ServiceProviderUpdate::State {
        instance: None,
        member: "left".to_string(),
        sequence: 3,
        ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "revision": 3 })))],
    };
    assert_eq!(
        first_decoder
            .decode_update(&first_encoder.encode_update(&left_base).unwrap())
            .unwrap(),
        left_base
    );
    let third_right = first_encoder.encode_update(&make_update("right", 3, 3)).unwrap();
    assert_eq!(serde_json::to_value(&third_right).unwrap()["ops"], json!([["s", 0, 3]]));
    assert_eq!(
        first_decoder.decode_update(&third_right).unwrap(),
        make_update("right", 3, 3)
    );

    let _unused = (
        WireServiceSubscriptionSnapshot {
            service_id: String::new(),
            mode: ServiceMode::Singleton,
            instances: Vec::new(),
        },
        WireServiceInstanceSnapshot {
            instance: None,
            members: Vec::new(),
        },
        WireServiceMemberSnapshot::State(WireServiceStateSnapshot {
            name: String::new(),
            kind: WireServiceStateKind::State,
            sequence: 0,
            ops: Vec::new(),
        }),
        WireOpSet(SetVerb::S, pi_chord::delta::PathRef::Id(0), json!(null)),
    );
}

#[test]
fn creates_and_removes_keyed_instance_codecs_with_their_lifecycle() {
    let enc = create_service_state_encoder();
    let dec = create_service_state_decoder();
    let snapshot = ServiceSubscriptionSnapshot {
        service_id: "pi.dialogs".to_string(),
        mode: ServiceMode::Keyed,
        instances: Vec::new(),
    };
    assert_eq!(
        dec.decode_snapshot(&enc.encode_snapshot(&snapshot).unwrap()).unwrap(),
        snapshot
    );

    let address = ServiceInstanceAddress {
        key: "dialog-1".to_string(),
        generation: 1,
    };
    let spawned = ServiceProviderUpdate::Spawned {
        instance: ServiceInstanceSnapshot {
            instance: Some(address.clone()),
            members: vec![ServiceMemberSnapshot::State(ServiceStateSnapshot {
                name: "request".to_string(),
                kind: ServiceMemberStateKind::State,
                sequence: 0,
                ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, json!({ "value": 0 })))],
            })],
        },
    };
    assert_eq!(
        dec.decode_update(&enc.encode_update(&spawned).unwrap()).unwrap(),
        spawned
    );

    let update = ServiceProviderUpdate::State {
        instance: Some(address.clone()),
        member: "request".to_string(),
        sequence: 1,
        ops: vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
    };
    assert_eq!(dec.decode_update(&enc.encode_update(&update).unwrap()).unwrap(), update);

    let closed = ServiceProviderUpdate::Closed { instance: address };
    assert_eq!(dec.decode_update(&enc.encode_update(&closed).unwrap()).unwrap(), closed);

    let stale = match update {
        ServiceProviderUpdate::State {
            instance, member, ops, ..
        } => ServiceProviderUpdate::State {
            instance,
            member,
            sequence: 2,
            ops,
        },
        _ => unreachable!(),
    };
    let err = enc.encode_update(&stale).unwrap_err();
    assert!(err.message().contains("Unknown service state"), "got: {err}");
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

#[tokio::test]
async fn remote_service_endpoints_publish_and_clean_up_provider_subscriptions() {
    let counter_service = define_service("test.counter", None).unwrap();
    let provider =
        RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(counter_service.clone())]).unwrap();
    let state = replicated_state(json!({ "value": 0 })).unwrap();

    let mut members = indexmap::IndexMap::new();
    members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(&state)));
    provider
        .provide(&counter_service, Arc::new(ServiceObject::from_members(members)))
        .unwrap();

    let endpoint = create_remote_service_endpoint(&provider);
    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let updates_cb = Arc::clone(&updates);
    let publish: pi_chord::ServiceUpdatePublisher = Arc::new(move |_subscription_id, update, _context| {
        updates_cb.lock().unwrap().push(update);
        Box::pin(futures::future::ready(Ok(())))
    });

    let catalogue_call = ServiceCall {
        service_id: "$chord.service".to_string(),
        instance: None,
        member: "catalogue".to_string(),
        args: Vec::new(),
    };
    let catalogue = endpoint
        .invoke(&catalogue_call, Arc::clone(&publish), BACKGROUND_CONTEXT.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        catalogue,
        json!([{ "serviceId": counter_service.id, "mode": "singleton" }])
    );

    let sub_call = create_service_subscribe_call("subscription-1", &counter_service.id, ServiceMode::Singleton);
    let sub_result = endpoint
        .invoke(&sub_call, Arc::clone(&publish), BACKGROUND_CONTEXT.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sub_result["serviceId"], json!(counter_service.id));
    assert_eq!(sub_result["mode"], json!("singleton"));

    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(1);
        })
        .unwrap();
    assert_eq!(
        *updates.lock().unwrap(),
        vec![ServiceProviderUpdate::State {
            instance: None,
            member: "state".to_string(),
            sequence: 1,
            ops: vec![Op::Set(OpSet(SetVerb::S, vec![Seg::String("value".into())], json!(1)))],
        }]
    );

    endpoint.dispose();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(2);
        })
        .unwrap();
    assert_eq!(updates.lock().unwrap().len(), 1);
    provider.dispose().unwrap();
}

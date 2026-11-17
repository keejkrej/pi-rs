//! Port of packages/chord/test/facets.test.ts

mod support;

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::provider::RemoteServiceProviderEntry;
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{
    Context, Facet, FacetOptions, JsonValue, MutableReplicatedState, RemoteServiceBindingOptions,
    RemoteServiceProvider, RemoteServiceSource, RemoteServices, Service, ServiceCatalogueEntry, ServiceMode,
    ServiceProviderUpdate, create_facet_host, create_remote_service_binding, define_facet, define_service,
};
use serde_json::json;
use support::helpers::create_loopback_service_transport;

fn svc_source() -> Service {
    define_service("test.experimental.source", None).unwrap()
}

fn svc_projection() -> Service {
    define_service("test.experimental.projection", None).unwrap()
}

fn svc_keyed_value() -> Service {
    define_service("test.experimental.keyed-value", None).unwrap()
}

fn svc_watched() -> Service {
    define_service("test.experimental.watched", None).unwrap()
}

fn svc_host_values() -> Service {
    define_service(
        "test.experimental.host-values",
        Some(DefineServiceOptions { local: Some(true) }),
    )
    .unwrap()
}

fn svc_local_keyed_value() -> Service {
    define_service(
        "test.experimental.local-keyed-value",
        Some(DefineServiceOptions { local: Some(true) }),
    )
    .unwrap()
}

fn svc_left_value() -> Service {
    define_service("test.experimental.left-value", None).unwrap()
}

fn svc_right_value() -> Service {
    define_service("test.experimental.right-value", None).unwrap()
}

fn svc_combined_value() -> Service {
    define_service("test.experimental.combined-value", None).unwrap()
}

fn make_read_service(
    read_fn: impl Fn(Context) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync + 'static,
) -> ServiceObject {
    let read_arc = Arc::new(read_fn);
    let mut members = IndexMap::new();
    members.insert(
        "read".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
            let ctx = args
                .first()
                .and_then(|v| match v {
                    ServiceValue::Object(obj) => obj.as_context(),
                    _ => None,
                })
                .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
            Ok(ServiceValue::Object(ServiceObject::from_promise((read_arc)(ctx))))
        })),
    );
    ServiceObject::from_members(members)
}

async fn call_read(service: &ServiceObject, context: &Context) -> pi_js::Result<String> {
    let ServiceValue::Object(read_fn) = service.get_key("read")? else {
        panic!("expected callable read member");
    };
    let result = read_fn.apply(
        None,
        &[ServiceValue::Object(ServiceObject::from_context(context.clone()))],
    )?;
    let ServiceValue::Object(promise) = result else {
        panic!("expected promise from read()");
    };
    let value = promise
        .into_json_future()
        .expect("read() must return promise")
        .await?
        .expect("read() must resolve to JSON string");
    Ok(value.as_str().expect("string result").to_string())
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
        if key == "subscribe" {
            let state = self.0.clone();
            return Ok(ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                let Some(ServiceValue::Object(cb)) = args.first() else {
                    return Err(pi_js::Error::js("TypeError", "subscribe listener must be callable"));
                };
                if !cb.is_callable() {
                    return Err(pi_js::Error::js("TypeError", "subscribe listener must be callable"));
                }
                let callback = cb.clone();
                let unsub = state.subscribe(Arc::new(move |val, ctx, delivery| {
                    let cb = callback.clone();
                    Box::pin(async move {
                        let del_json =
                            serde_json::to_value(delivery).map_err(|e| pi_js::Error::js("Error", e.to_string()))?;
                        let res = cb.apply(
                            None,
                            &[
                                ServiceValue::from_json(&val),
                                ServiceValue::Object(ServiceObject::from_context(ctx)),
                                ServiceValue::from_json(&del_json),
                            ],
                        )?;
                        if let ServiceValue::Object(obj) = res {
                            if let Some(fut) = obj.into_json_future() {
                                let _ = fut.await?;
                            }
                        }
                        Ok(())
                    })
                }));
                let cell = Arc::new(Mutex::new(Some(unsub)));
                Ok(ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
                    if let Some(u) = cell.lock().unwrap().take() {
                        u.call();
                    }
                    Ok(ServiceValue::Undefined)
                })))
            })));
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

async fn wait_for(cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition was not satisfied in time");
}

#[tokio::test]
async fn discovers_setup_dependencies_before_connecting_stable_service_handles() {
    let source_svc = svc_source();
    let projection_svc = svc_projection();
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let source_handle: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));

    let trace_proj = Arc::clone(&trace);
    let src_handle_slot = Arc::clone(&source_handle);
    let src_svc_p = source_svc.clone();
    let proj_svc_p = projection_svc.clone();
    let projection = define_facet(Facet::new(
        "projection",
        Arc::new(move |env| {
            trace_proj.lock().unwrap().push("setup projection".to_string());
            let handle = env.r#use::<ServiceObject>(&src_svc_p)?;
            let call_err = handle.get_key("read").err().expect("expected setting_up error");
            assert!(
                call_err
                    .to_js_string()
                    .contains("Facet projection service handles cannot be used while setting_up"),
                "unexpected error: {}",
                call_err.to_js_string()
            );
            *src_handle_slot.lock().unwrap() = Some(handle.clone());
            let handle_for_read = handle;
            env.provide(
                &proj_svc_p,
                make_read_service(move |ctx| {
                    let h = handle_for_read.clone();
                    Box::pin(async move {
                        let s = call_read(&h, &ctx).await?;
                        Ok(Some(json!(s)))
                    })
                }),
            )?;
            let trace_act = Arc::clone(&trace_proj);
            env.on_activate(Arc::new(move || {
                trace_act.lock().unwrap().push("activate projection".to_string());
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            let trace_deact = Arc::clone(&trace_proj);
            env.on_deactivate(Arc::new(move || {
                trace_deact.lock().unwrap().push("dispose projection".to_string());
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            Ok(())
        }),
    ));

    let trace_src = Arc::clone(&trace);
    let src_svc_s = source_svc.clone();
    let source = define_facet(Facet::new(
        "source",
        Arc::new(move |env| {
            trace_src.lock().unwrap().push("setup source".to_string());
            env.provide(
                &src_svc_s,
                make_read_service(|_ctx| Box::pin(futures::future::ready(Ok(Some(json!("value")))))),
            )?;
            let trace_act = Arc::clone(&trace_src);
            env.on_activate(Arc::new(move || {
                trace_act.lock().unwrap().push("activate source".to_string());
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            let trace_deact = Arc::clone(&trace_src);
            env.on_deactivate(Arc::new(move || {
                trace_deact.lock().unwrap().push("dispose source".to_string());
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            Ok(())
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![projection, source],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    assert_eq!(
        *trace.lock().unwrap(),
        vec![
            "setup projection",
            "setup source",
            "activate source",
            "activate projection",
        ]
    );

    let handle = source_handle.lock().unwrap().clone().unwrap();
    assert_eq!(call_read(&handle, &BACKGROUND_CONTEXT).await.unwrap(), "value");
    let host_proj = host.services().use_::<ServiceObject>(&projection_svc).unwrap();
    assert_eq!(call_read(&host_proj, &BACKGROUND_CONTEXT).await.unwrap(), "value");

    host.dispose().await.unwrap();
    let t = trace.lock().unwrap();
    assert_eq!(&t[t.len() - 2..], &["dispose projection", "dispose source"]);
}

#[tokio::test]
async fn connects_keyed_observations_only_when_the_observing_facet_activates() {
    let keyed_value = svc_keyed_value();
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let trace_obs = Arc::clone(&trace);
    let kv_obs = keyed_value.clone();
    let observer = define_facet(Facet::new(
        "observer",
        Arc::new(move |env| {
            let trace_cb = Arc::clone(&trace_obs);
            env.observe::<ServiceObject>(
                &kv_obs,
                Arc::new(move |service, ctx| {
                    let trace_inner = Arc::clone(&trace_cb);
                    Box::pin(async move {
                        let value = call_read(&service, &ctx).await?;
                        trace_inner.lock().unwrap().push(format!("observe {value}"));
                        Ok(())
                    })
                }),
            )?;
            let trace_act = Arc::clone(&trace_obs);
            env.on_activate(Arc::new(move || {
                trace_act.lock().unwrap().push("activate observer".to_string());
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            Ok(())
        }),
    ));

    let trace_prov = Arc::clone(&trace);
    let kv_prov = keyed_value.clone();
    let provider = define_facet(Facet::new(
        "provider",
        Arc::new(move |env| {
            let values = env.provide_many::<ServiceObject>(&kv_prov)?;
            let trace_act = Arc::clone(&trace_prov);
            env.on_activate(Arc::new(move || {
                trace_act.lock().unwrap().push("activate provider".to_string());
                values.spawn(
                    "one",
                    make_read_service(|_ctx| Box::pin(futures::future::ready(Ok(Some(json!("one")))))),
                );
                Box::pin(futures::future::ready(Ok(())))
            }))?;
            Ok(())
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![observer, provider],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    wait_for(|| trace.lock().unwrap().iter().any(|item| item == "observe one")).await;
    assert_eq!(
        *trace.lock().unwrap(),
        vec!["activate provider", "activate observer", "observe one"]
    );

    let remote_services = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: keyed_value.id.clone(),
        }],
        transport: create_loopback_service_transport(&host.services()),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();

    let remote_values: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let rv_cb = Arc::clone(&remote_values);
    let _stop = remote_services
        .observe::<ServiceObject>(
            &keyed_value,
            Arc::new(move |service, ctx| {
                let rv = Arc::clone(&rv_cb);
                Box::pin(async move {
                    let s = call_read(&service, &ctx).await?;
                    rv.lock().unwrap().push(s);
                    Ok(())
                })
            }),
        )
        .unwrap();
    remote_services.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    wait_for(|| *remote_values.lock().unwrap() == vec!["one"]).await;

    remote_services.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn routes_remotely_exposable_keyed_services_through_the_host_provider() {
    let keyed_value = svc_keyed_value();
    let observed: Arc<Mutex<Vec<(ServiceObject, Context)>>> = Arc::new(Mutex::new(Vec::new()));

    let obs_clone = Arc::clone(&observed);
    let kv_c = keyed_value.clone();
    let consumer = define_facet(Facet::new(
        "remote-keyed-consumer",
        Arc::new(move |env| {
            let obs_cb = Arc::clone(&obs_clone);
            env.observe::<ServiceObject>(
                &kv_c,
                Arc::new(move |service, ctx| {
                    obs_cb.lock().unwrap().push((service, ctx));
                    Box::pin(futures::future::ready(Ok(())))
                }),
            )
        }),
    ));

    let make_provider = |value: &'static str| {
        let kv_p = keyed_value.clone();
        define_facet(Facet::new(
            "remote-keyed-provider",
            Arc::new(move |env| {
                let values = env.provide_many::<ServiceObject>(&kv_p)?;
                env.on_activate(Arc::new(move || {
                    values.spawn(
                        "current",
                        make_read_service(move |_ctx| Box::pin(futures::future::ready(Ok(Some(json!(value)))))),
                    );
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, make_provider("A")],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    wait_for(|| observed.lock().unwrap().len() == 1).await;
    let first = observed.lock().unwrap()[0].clone();
    assert_eq!(call_read(&first.0, &first.1).await.unwrap(), "A");

    host.reload(vec![make_provider("B")]).await.unwrap();
    wait_for(|| observed.lock().unwrap().len() == 2).await;
    assert!(first.1.abort_signal().unwrap().aborted());

    let closed_read = first.0.get_key("read").err().expect("expected closed read error");
    assert!(closed_read.to_js_string().contains("observation is closed"));

    let second = observed.lock().unwrap()[1].clone();
    assert_eq!(call_read(&second.0, &second.1).await.unwrap(), "B");

    host.dispose().await.unwrap();
    assert!(second.1.abort_signal().unwrap().aborted());
}

#[tokio::test]
async fn terminates_the_host_when_keyed_replacement_publication_fails() {
    let keyed_value = svc_keyed_value();
    let make_provider = |value: &'static str| {
        let kv_p = keyed_value.clone();
        define_facet(Facet::new(
            "failing-keyed-provider",
            Arc::new(move |env| {
                let values = env.provide_many::<ServiceObject>(&kv_p)?;
                env.on_activate(Arc::new(move || {
                    values.spawn(
                        "current",
                        make_read_service(move |_ctx| Box::pin(futures::future::ready(Ok(Some(json!(value)))))),
                    );
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![make_provider("A")],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let subscription = host
        .services()
        .subscribe_fallible(&keyed_value.id, ServiceMode::Keyed, |update, _ctx| {
            if matches!(update, ServiceProviderUpdate::Spawned { .. }) {
                return Err(pi_js::Error::js("Error", "spawn publication failed"));
            }
            Ok(())
        })
        .unwrap();
    subscription.activate().unwrap();

    let err = host.reload(vec![make_provider("B")]).await.unwrap_err();
    assert!(
        err.to_js_string().contains("Facet reload failed after cutover"),
        "unexpected reload error: {}",
        err.to_js_string()
    );

    let err_dead = host.reload(vec![]).await.unwrap_err();
    assert!(err_dead.to_js_string().contains("Facet host cannot reload while dead"));

    let err_use = host
        .services()
        .use_::<ServiceObject>(&keyed_value)
        .err()
        .expect("expected disposed provider error");
    assert!(err_use.to_js_string().contains("Remote service provider is disposed"));

    host.dispose().await.unwrap();
}

#[tokio::test]
async fn terminates_the_host_when_keyed_retirement_publication_fails() {
    let keyed_value = svc_keyed_value();
    let make_provider = |value: &'static str| {
        let kv_p = keyed_value.clone();
        define_facet(Facet::new(
            "failing-keyed-retirement-provider",
            Arc::new(move |env| {
                let values = env.provide_many::<ServiceObject>(&kv_p)?;
                env.on_activate(Arc::new(move || {
                    values.spawn(
                        "current",
                        make_read_service(move |_ctx| Box::pin(futures::future::ready(Ok(Some(json!(value)))))),
                    );
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![make_provider("A")],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let subscription = host
        .services()
        .subscribe_fallible(&keyed_value.id, ServiceMode::Keyed, |update, _ctx| {
            if matches!(update, ServiceProviderUpdate::Closed { .. }) {
                return Err(pi_js::Error::js("Error", "close publication failed"));
            }
            Ok(())
        })
        .unwrap();
    subscription.activate().unwrap();

    let err = host.reload(vec![make_provider("B")]).await.unwrap_err();
    assert!(err.to_js_string().contains("Facet reload failed after cutover"));

    let err_dead = host.reload(vec![]).await.unwrap_err();
    assert!(err_dead.to_js_string().contains("Facet host cannot reload while dead"));

    host.dispose().await.unwrap();
}

#[tokio::test]
async fn keeps_unrestricted_local_keyed_services_process_local_across_provider_reloads() {
    let local_keyed_value = svc_local_keyed_value();
    let observed: Arc<Mutex<Vec<(ServiceObject, Context)>>> = Arc::new(Mutex::new(Vec::new()));

    let obs_clone = Arc::clone(&observed);
    let lk_c = local_keyed_value.clone();
    let consumer = define_facet(Facet::new(
        "local-keyed-consumer",
        Arc::new(move |env| {
            let obs_cb = Arc::clone(&obs_clone);
            env.observe::<ServiceObject>(
                &lk_c,
                Arc::new(move |service, ctx| {
                    obs_cb.lock().unwrap().push((service, ctx));
                    Box::pin(futures::future::ready(Ok(())))
                }),
            )
        }),
    ));

    let make_local_item = |value: &'static str| {
        let mut meta = IndexMap::new();
        meta.insert("value".to_string(), value.to_string());
        let meta_arc = Arc::new(meta);
        let mut meta_members = IndexMap::new();
        meta_members.insert(
            "get".to_string(),
            ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                let key = args.first().and_then(|v| match v {
                    ServiceValue::String(s) => Some(s.as_str()),
                    _ => None,
                });
                match key.and_then(|k| meta_arc.get(k)) {
                    Some(v) => Ok(ServiceValue::String(v.clone())),
                    None => Ok(ServiceValue::Undefined),
                }
            })),
        );
        let mut members = IndexMap::new();
        members.insert(
            "metadata".to_string(),
            ServiceValue::Object(ServiceObject::from_members(meta_members)),
        );
        members.insert(
            "read".to_string(),
            ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
                Ok(ServiceValue::String(value.to_string()))
            })),
        );
        ServiceObject::from_members(members)
    };

    let make_provider = |value: &'static str| {
        let lk_p = local_keyed_value.clone();
        define_facet(Facet::new(
            "local-keyed-provider",
            Arc::new(move |env| {
                let values = env.provide_many::<ServiceObject>(&lk_p)?;
                env.on_activate(Arc::new(move || {
                    values.spawn("current", make_local_item(value));
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, make_provider("A")],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    wait_for(|| observed.lock().unwrap().len() == 1).await;
    let first = observed.lock().unwrap()[0].clone();
    let call_local_read = |svc: &ServiceObject| -> pi_js::Result<String> {
        let ServiceValue::Object(read_fn) = svc.get_key("read")? else {
            panic!("expected callable read");
        };
        match read_fn.apply(None, &[])? {
            ServiceValue::String(s) => Ok(s),
            _ => panic!("expected string ServiceValue"),
        }
    };
    let get_meta_value = |svc: &ServiceObject| -> String {
        let ServiceValue::Object(meta) = svc.get_key("metadata").unwrap() else {
            panic!("expected metadata");
        };
        let ServiceValue::Object(get_fn) = meta.get_key("get").unwrap() else {
            panic!("expected get");
        };
        match get_fn
            .apply(None, &[ServiceValue::String("value".to_string())])
            .unwrap()
        {
            ServiceValue::String(s) => s,
            _ => panic!("expected string ServiceValue"),
        }
    };

    assert_eq!(call_local_read(&first.0).unwrap(), "A");
    assert_eq!(get_meta_value(&first.0), "A");
    assert!(!host.services().catalogue().contains(&ServiceCatalogueEntry {
        service_id: local_keyed_value.id.clone(),
        mode: ServiceMode::Keyed,
    }));
    let err_use = host
        .services()
        .use_::<ServiceObject>(&local_keyed_value)
        .err()
        .expect("expected process-local error");
    assert!(err_use.to_js_string().contains("process-local"));

    host.reload(vec![make_provider("B")]).await.unwrap();
    wait_for(|| observed.lock().unwrap().len() == 2).await;
    assert!(first.1.abort_signal().unwrap().aborted());
    let err_closed = call_local_read(&first.0).unwrap_err();
    assert!(
        err_closed
            .to_js_string()
            .contains(&format!("Keyed service {} observation is closed", local_keyed_value.id)),
        "unexpected closed error: {}",
        err_closed.to_js_string()
    );

    let second = observed.lock().unwrap()[1].clone();
    assert_eq!(call_local_read(&second.0).unwrap(), "B");
    assert_eq!(get_meta_value(&second.0), "B");

    host.dispose().await.unwrap();
    assert!(second.1.abort_signal().unwrap().aborted());
}

#[tokio::test]
async fn keeps_remotely_exposable_local_state_replicas_stable_across_provider_reloads() {
    let watched_svc = svc_watched();
    let sources: Arc<Mutex<Vec<MutableReplicatedState<JsonValue>>>> = Arc::new(Mutex::new(Vec::new()));
    let revisions: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let watched_slot: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));

    let ws = Arc::clone(&watched_slot);
    let rev = Arc::clone(&revisions);
    let w_c = watched_svc.clone();
    let consumer = define_facet(Facet::new(
        "state-consumer",
        Arc::new(move |env| {
            let watched = env.r#use::<ServiceObject>(&w_c)?;
            *ws.lock().unwrap() = Some(watched.clone());
            let rev_act = Arc::clone(&rev);
            let env_own = env.clone();
            env.on_activate(Arc::new(move || {
                let watched = watched.clone();
                let rev_act = Arc::clone(&rev_act);
                let env_own = env_own.clone();
                Box::pin(async move {
                    let ServiceValue::Object(state_obj) = watched.get_key("state")? else {
                        panic!("expected state member");
                    };
                    let ServiceValue::Object(sub_fn) = state_obj.get_key("subscribe")? else {
                        panic!("expected subscribe member");
                    };
                    let rev_cb = Arc::clone(&rev_act);
                    let listener = ServiceObject::from_fn(move |_this, args| {
                        if let Some(val) = args.first() {
                            let json = val.to_json()?;
                            if let Some(n) = json.get("value").and_then(|v| v.as_i64()) {
                                rev_cb.lock().unwrap().push(n);
                            }
                        }
                        Ok(ServiceValue::Undefined)
                    });
                    let ServiceValue::Object(unsub) = sub_fn.apply(None, &[ServiceValue::Object(listener)])? else {
                        panic!("expected unsubscribe callable");
                    };
                    env_own.own(Arc::new(move || {
                        let unsub = unsub.clone();
                        Box::pin(async move {
                            unsub.apply(None, &[])?;
                            Ok(())
                        })
                    }))?;
                    Ok(())
                })
            }))?;
            Ok(())
        }),
    ));

    let make_provider = |value: i64| {
        let srcs = Arc::clone(&sources);
        let w_p = watched_svc.clone();
        define_facet(Facet::new(
            "state-provider",
            Arc::new(move |env| {
                let state = env.replicated_state(json!({ "value": value }))?;
                srcs.lock().unwrap().push(state.clone());
                let mut members = IndexMap::new();
                members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(&state)));
                env.provide(&w_p, ServiceObject::from_members(members))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, make_provider(1)],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let watched = watched_slot.lock().unwrap().clone().unwrap();
    let ServiceValue::Object(retained_state) = watched.get_key("state").unwrap() else {
        panic!("expected state handle");
    };
    assert_eq!(
        retained_state.get_key("value").unwrap().to_json().unwrap(),
        json!({ "value": 1 })
    );
    assert_eq!(*revisions.lock().unwrap(), vec![1]);

    host.reload(vec![make_provider(2)]).await.unwrap();
    let ServiceValue::Object(reloaded_state) = watched.get_key("state").unwrap() else {
        panic!("expected state handle after reload");
    };
    assert!(reloaded_state.ptr_eq(&retained_state));
    assert_eq!(
        retained_state.get_key("value").unwrap().to_json().unwrap(),
        json!({ "value": 2 })
    );
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2]);

    sources.lock().unwrap()[0]
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(3);
        })
        .unwrap();
    assert_eq!(
        retained_state.get_key("value").unwrap().to_json().unwrap(),
        json!({ "value": 2 })
    );
    assert_eq!(*revisions.lock().unwrap(), vec![1, 2]);

    host.dispose().await.unwrap();
    let err_dead = retained_state
        .get_key("value")
        .err()
        .expect("expected dead state error");
    assert!(
        err_dead
            .to_js_string()
            .contains("Facet state-consumer service handles cannot be used while dead"),
        "unexpected dead error: {}",
        err_dead.to_js_string()
    );
}

#[tokio::test]
async fn scopes_singleton_service_views_to_each_facet_lifecycle() {
    let source_svc = svc_source();
    let consumer_handles: Arc<Mutex<Vec<ServiceObject>>> = Arc::new(Mutex::new(Vec::new()));
    let cleanup_values: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let peer_handle: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));

    let make_consumer = |generation: &'static str| {
        let ch = Arc::clone(&consumer_handles);
        let cv = Arc::clone(&cleanup_values);
        let src_c = source_svc.clone();
        define_facet(Facet::new(
            "scoped-consumer",
            Arc::new(move |env| {
                let source = env.r#use::<ServiceObject>(&src_c)?;
                let second_use = env.r#use::<ServiceObject>(&src_c)?;
                assert!(source.ptr_eq(&second_use));
                ch.lock().unwrap().push(source.clone());

                let setup_err = source.get_key("read").err().expect("expected setting_up error");
                assert!(
                    setup_err
                        .to_js_string()
                        .contains("Facet scoped-consumer service handles cannot be used while setting_up")
                );

                let cv_deact = Arc::clone(&cv);
                let src_deact = source.clone();
                env.on_deactivate(Arc::new(move || {
                    let cv_inner = Arc::clone(&cv_deact);
                    let src_inner = src_deact.clone();
                    Box::pin(async move {
                        let val = call_read(&src_inner, &BACKGROUND_CONTEXT).await?;
                        cv_inner.lock().unwrap().push(format!("{generation}:{val}"));
                        Ok(())
                    })
                }))?;
                Ok(())
            }),
        ))
    };

    let ph = Arc::clone(&peer_handle);
    let src_peer = source_svc.clone();
    let peer = define_facet(Facet::new(
        "peer-consumer",
        Arc::new(move |env| {
            *ph.lock().unwrap() = Some(env.r#use::<ServiceObject>(&src_peer)?);
            Ok(())
        }),
    ));

    let src_prov = source_svc.clone();
    let provider = define_facet(Facet::new(
        "scoped-provider",
        Arc::new(move |env| {
            env.provide(
                &src_prov,
                make_read_service(|_ctx| Box::pin(futures::future::ready(Ok(Some(json!("value")))))),
            )
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![make_consumer("A"), peer, provider],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let peer_obj = peer_handle.lock().unwrap().clone().unwrap();
    let first_handle = consumer_handles.lock().unwrap()[0].clone();
    assert!(!first_handle.ptr_eq(&peer_obj));
    let ServiceValue::Object(retained_old_read) = first_handle.get_key("read").unwrap() else {
        panic!("expected read member");
    };

    host.reload(vec![make_consumer("B")]).await.unwrap();
    assert_eq!(*cleanup_values.lock().unwrap(), vec!["A:value"]);
    let second_handle = consumer_handles.lock().unwrap()[1].clone();
    assert!(!second_handle.ptr_eq(&first_handle));

    let dead_read_1 = first_handle.get_key("read").err().expect("expected dead error");
    assert!(
        dead_read_1
            .to_js_string()
            .contains("Facet scoped-consumer service handles cannot be used while dead")
    );

    let dead_read_2 = retained_old_read
        .apply(
            None,
            &[ServiceValue::Object(ServiceObject::from_context(
                BACKGROUND_CONTEXT.clone(),
            ))],
        )
        .err()
        .expect("expected dead error");
    assert!(
        dead_read_2
            .to_js_string()
            .contains("Facet scoped-consumer service handles cannot be used while dead")
    );

    assert_eq!(call_read(&second_handle, &BACKGROUND_CONTEXT).await.unwrap(), "value");
    assert_eq!(call_read(&peer_obj, &BACKGROUND_CONTEXT).await.unwrap(), "value");

    host.dispose().await.unwrap();
    assert_eq!(*cleanup_values.lock().unwrap(), vec!["A:value", "B:value"]);

    let dead_second = second_handle.get_key("read").err().expect("expected dead error");
    assert!(
        dead_second
            .to_js_string()
            .contains("Facet scoped-consumer service handles cannot be used while dead")
    );

    let dead_peer = peer_obj.get_key("read").err().expect("expected dead error");
    assert!(
        dead_peer
            .to_js_string()
            .contains("Facet peer-consumer service handles cannot be used while dead")
    );
}

#[tokio::test]
async fn owns_resources_registered_during_activation() {
    let watched_svc = svc_watched();
    let state_slot: Arc<Mutex<Option<MutableReplicatedState<JsonValue>>>> = Arc::new(Mutex::new(None));
    let deliveries = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let del_c = Arc::clone(&deliveries);
    let w_c = watched_svc.clone();
    let consumer = define_facet(Facet::new(
        "consumer",
        Arc::new(move |env| {
            let watched = env.r#use::<ServiceObject>(&w_c)?;
            let del_act = Arc::clone(&del_c);
            let env_own = env.clone();
            env.on_activate(Arc::new(move || {
                let watched = watched.clone();
                let del_act = Arc::clone(&del_act);
                let env_own = env_own.clone();
                Box::pin(async move {
                    let ServiceValue::Object(state_obj) = watched.get_key("state")? else {
                        panic!("expected state");
                    };
                    let ServiceValue::Object(sub_fn) = state_obj.get_key("subscribe")? else {
                        panic!("expected subscribe");
                    };
                    let del_cb = Arc::clone(&del_act);
                    let listener = ServiceObject::from_fn(move |_this, _args| {
                        del_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(ServiceValue::Undefined)
                    });
                    let ServiceValue::Object(unsub) = sub_fn.apply(None, &[ServiceValue::Object(listener)])? else {
                        panic!("expected unsub");
                    };
                    env_own.own(Arc::new(move || {
                        let unsub = unsub.clone();
                        Box::pin(async move {
                            unsub.apply(None, &[])?;
                            Ok(())
                        })
                    }))?;
                    Ok(())
                })
            }))?;
            Ok(())
        }),
    ));

    let ss = Arc::clone(&state_slot);
    let w_p = watched_svc.clone();
    let provider = define_facet(Facet::new(
        "provider",
        Arc::new(move |env| {
            let state = env.replicated_state(json!({ "value": 0 }))?;
            *ss.lock().unwrap() = Some(state.clone());
            let mut members = IndexMap::new();
            members.insert("state".to_string(), ServiceValue::Object(wrap_mutable_state(&state)));
            env.provide(&w_p, ServiceObject::from_members(members))
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, provider],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    assert_eq!(deliveries.load(std::sync::atomic::Ordering::SeqCst), 1);
    let state = state_slot.lock().unwrap().clone().unwrap();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(1);
        })
        .unwrap();
    assert_eq!(deliveries.load(std::sync::atomic::Ordering::SeqCst), 2);

    host.dispose().await.unwrap();
    state
        .change(BACKGROUND_CONTEXT.clone(), |draft| {
            draft["value"] = json!(2);
        })
        .unwrap();
    assert_eq!(deliveries.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn provides_arbitrary_host_services_through_the_facet_graph() {
    let host_values_svc = svc_host_values();
    let mut members = IndexMap::new();
    members.insert("name".to_string(), ServiceValue::String("session".to_string()));
    members.insert("use".to_string(), ServiceValue::String("host value".to_string()));
    let values = ServiceObject::from_members(members);

    let hv_c = host_values_svc.clone();
    let vals_c = values.clone();
    let consumer = define_facet(Facet::new(
        "host-service-consumer",
        Arc::new(move |env| {
            let host_values = env.r#use::<ServiceObject>(&hv_c)?;
            let err = host_values.get_key("use").err().expect("expected setting_up error");
            assert!(
                err.to_js_string()
                    .contains("Facet host-service-consumer service handles cannot be used while setting_up")
            );
            let hv_act = host_values;
            let vals_act = vals_c.clone();
            env.on_activate(Arc::new(move || {
                let hv_act = hv_act.clone();
                let vals_act = vals_act.clone();
                Box::pin(async move {
                    assert!(!hv_act.ptr_eq(&vals_act));
                    match hv_act.get_key("name")? {
                        ServiceValue::String(s) => assert_eq!(s, "session"),
                        _ => panic!("expected name string"),
                    }
                    match hv_act.get_key("use")? {
                        ServiceValue::String(s) => assert_eq!(s, "host value"),
                        _ => panic!("expected use string"),
                    }
                    Ok(())
                })
            }))?;
            Ok(())
        }),
    ));

    let hv_p = host_values_svc.clone();
    let vals_p = values.clone();
    let provider = define_facet(Facet::new(
        "host-service-provider",
        Arc::new(move |env| env.provide(&hv_p, vals_p.clone())),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, provider],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let err_use = host
        .services()
        .use_::<ServiceObject>(&host_values_svc)
        .err()
        .expect("expected process-local error");
    assert!(
        err_use
            .to_js_string()
            .contains(&format!("Service {} is process-local", host_values_svc.id))
    );
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn combines_connected_services_and_facet_provided_services_in_one_host() {
    let left_value = svc_left_value();
    let right_value = svc_right_value();
    let combined_value = svc_combined_value();

    let left_provider =
        RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(left_value.clone())]).unwrap();
    left_provider
        .provide(
            &left_value,
            Arc::new(make_read_service(|_ctx| {
                Box::pin(futures::future::ready(Ok(Some(json!("left")))))
            })),
        )
        .unwrap();

    let right_provider =
        RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(right_value.clone())]).unwrap();
    right_provider
        .provide(
            &right_value,
            Arc::new(make_read_service(|_ctx| {
                Box::pin(futures::future::ready(Ok(Some(json!("right")))))
            })),
        )
        .unwrap();

    let left_namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: left_value.id.clone(),
        }],
        transport: create_loopback_service_transport(&left_provider),
        bound: Some(false),
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let right_namespace = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: right_value.id.clone(),
        }],
        transport: create_loopback_service_transport(&right_provider),
        bound: Some(false),
        on_error: None,
        assert_access: None,
    })
    .unwrap();

    let make_source = |namespace: pi_chord::RemoteServiceBinding, provider: RemoteServiceProvider| {
        RemoteServiceSource::new(
            false,
            Arc::new(move |_ctx| {
                let catalogue = provider.catalogue().to_vec();
                Box::pin(futures::future::ready(Ok(catalogue)))
            }),
            Arc::new(move |_opts| {
                let ns_use = namespace.clone();
                let ns_ready = namespace.clone();
                let ns_disp = namespace.clone();
                let as_rs = namespace.as_remote_services();
                RemoteServices::new(
                    Arc::new(move |svc| ns_use.r#use::<ServiceObject>(svc)),
                    Arc::new(move |svc, handler| as_rs.observe::<ServiceObject>(svc, handler)),
                    Arc::new(move |ctx| {
                        let ns = ns_ready.clone();
                        Box::pin(async move {
                            ns.rebind(true, ctx.clone()).await?;
                            ns.ready(ctx).await
                        })
                    }),
                    Arc::new(move |ctx| {
                        let ns = ns_disp.clone();
                        Box::pin(async move { ns.dispose(ctx).await })
                    }),
                )
            }),
        )
    };

    let service_sources = vec![
        make_source(left_namespace.clone(), left_provider.clone()),
        make_source(right_namespace.clone(), right_provider.clone()),
    ];

    let lv = left_value.clone();
    let rv = right_value.clone();
    let cv = combined_value.clone();
    let facet = define_facet(Facet::new(
        "combined",
        Arc::new(move |env| {
            let left = env.r#use::<ServiceObject>(&lv)?;
            let right = env.r#use::<ServiceObject>(&rv)?;
            env.provide(
                &cv,
                make_read_service(move |ctx| {
                    let l = left.clone();
                    let r = right.clone();
                    Box::pin(async move {
                        let l_val = call_read(&l, &ctx).await?;
                        let r_val = call_read(&r, &ctx).await?;
                        Ok(Some(json!(format!("{l_val} {r_val}"))))
                    })
                }),
            )
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![facet],
        service_sources: Some(service_sources),
        on_error: None,
    })
    .await
    .unwrap();

    let combined = host.services().use_::<ServiceObject>(&combined_value).unwrap();
    assert_eq!(call_read(&combined, &BACKGROUND_CONTEXT).await.unwrap(), "left right");

    host.dispose().await.unwrap();
    let _ = tokio::join!(
        left_namespace.dispose(BACKGROUND_CONTEXT.clone()),
        right_namespace.dispose(BACKGROUND_CONTEXT.clone())
    );
    left_provider.dispose().unwrap();
    right_provider.dispose().unwrap();
}

#[tokio::test]
async fn rejects_a_service_offered_by_multiple_sources() {
    let left_value = svc_left_value();
    let left_id = left_value.id.clone();
    let duplicate = RemoteServiceSource::new(
        false,
        Arc::new(move |_ctx| {
            let id = left_id.clone();
            Box::pin(futures::future::ready(Ok(vec![ServiceCatalogueEntry {
                service_id: id,
                mode: ServiceMode::Singleton,
            }])))
        }),
        Arc::new(|_opts| panic!("Ambiguous sources must not open")),
    );

    let lv = left_value.clone();
    let consumer = define_facet(Facet::new(
        "duplicate-consumer",
        Arc::new(move |env| {
            let _ = env.r#use::<ServiceObject>(&lv)?;
            Ok(())
        }),
    ));

    let err = create_facet_host(FacetOptions {
        facets: vec![consumer],
        service_sources: Some(vec![duplicate.clone(), duplicate]),
        on_error: None,
    })
    .await
    .err()
    .expect("duplicate source should reject");

    assert!(
        err.to_js_string().contains(&format!(
            "Facet host service {} is offered by more than one source",
            left_value.id
        )),
        "unexpected error: {}",
        err.to_js_string()
    );
}

#[tokio::test]
async fn reopens_source_bindings_from_changed_catalogues_for_a_replacement_generation() {
    let left_value = svc_left_value();
    let right_value = svc_right_value();

    let left_provider =
        RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(left_value.clone())]).unwrap();
    left_provider
        .provide(
            &left_value,
            Arc::new(make_read_service(|_ctx| {
                Box::pin(futures::future::ready(Ok(Some(json!("left")))))
            })),
        )
        .unwrap();

    let right_provider =
        RemoteServiceProvider::new(vec![RemoteServiceProviderEntry::Service(right_value.clone())]).unwrap();
    right_provider
        .provide(
            &right_value,
            Arc::new(make_read_service(|_ctx| {
                Box::pin(futures::future::ready(Ok(Some(json!("right")))))
            })),
        )
        .unwrap();

    let current_provider = Arc::new(Mutex::new(left_provider.clone()));
    let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let disposed = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let cp_cat = Arc::clone(&current_provider);
    let cp_open = Arc::clone(&current_provider);
    let opened_c = Arc::clone(&opened);
    let disposed_c = Arc::clone(&disposed);
    let source = RemoteServiceSource::new(
        false,
        Arc::new(move |_ctx| {
            let catalogue = cp_cat.lock().unwrap().catalogue().to_vec();
            Box::pin(futures::future::ready(Ok(catalogue)))
        }),
        Arc::new(move |options: RemoteServiceSourceOpenOptions| {
            opened_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let prov = cp_open.lock().unwrap().clone();
            let namespace = create_remote_service_binding(RemoteServiceBindingOptions {
                services: options.services,
                transport: create_loopback_service_transport(&prov),
                bound: Some(false),
                on_error: Some(options.on_error),
                assert_access: Some(options.assert_access),
            })
            .unwrap();
            let ns_use = namespace.clone();
            let ns_as_rs = namespace.as_remote_services();
            let ns_ready = namespace.clone();
            let ns_disp = namespace;
            let disp_cnt = Arc::clone(&disposed_c);
            RemoteServices::new(
                Arc::new(move |svc| ns_use.r#use::<ServiceObject>(svc)),
                Arc::new(move |svc, handler| ns_as_rs.observe::<ServiceObject>(svc, handler)),
                Arc::new(move |ctx| {
                    let ns = ns_ready.clone();
                    Box::pin(async move {
                        ns.rebind(true, ctx.clone()).await?;
                        ns.ready(ctx).await
                    })
                }),
                Arc::new(move |ctx| {
                    let ns = ns_disp.clone();
                    let d = Arc::clone(&disp_cnt);
                    Box::pin(async move {
                        d.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        ns.dispose(ctx).await
                    })
                }),
            )
        }),
    );

    let values: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let vals_l = Arc::clone(&values);
    let lv = left_value.clone();
    let left_consumer = define_facet(Facet::new(
        "left-consumer",
        Arc::new(move |env| {
            let left = env.r#use::<ServiceObject>(&lv)?;
            let vals_act = Arc::clone(&vals_l);
            env.on_activate(Arc::new(move || {
                let l = left.clone();
                let v = Arc::clone(&vals_act);
                Box::pin(async move {
                    let s = call_read(&l, &BACKGROUND_CONTEXT).await?;
                    v.lock().unwrap().push(s);
                    Ok(())
                })
            }))
        }),
    ));
    let first = create_facet_host(FacetOptions {
        facets: vec![left_consumer],
        service_sources: Some(vec![source.clone()]),
        on_error: None,
    })
    .await
    .unwrap();
    first.dispose().await.unwrap();

    *current_provider.lock().unwrap() = right_provider.clone();
    let vals_r = Arc::clone(&values);
    let rv = right_value.clone();
    let right_consumer = define_facet(Facet::new(
        "right-consumer",
        Arc::new(move |env| {
            let right = env.r#use::<ServiceObject>(&rv)?;
            let vals_act = Arc::clone(&vals_r);
            env.on_activate(Arc::new(move || {
                let r = right.clone();
                let v = Arc::clone(&vals_act);
                Box::pin(async move {
                    let s = call_read(&r, &BACKGROUND_CONTEXT).await?;
                    v.lock().unwrap().push(s);
                    Ok(())
                })
            }))
        }),
    ));
    let second = create_facet_host(FacetOptions {
        facets: vec![right_consumer],
        service_sources: Some(vec![source]),
        on_error: None,
    })
    .await
    .unwrap();
    second.dispose().await.unwrap();

    assert_eq!(*values.lock().unwrap(), vec!["left", "right"]);
    assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(disposed.load(std::sync::atomic::Ordering::SeqCst), 2);
    left_provider.dispose().unwrap();
    right_provider.dispose().unwrap();
}

#[tokio::test]
async fn rejects_missing_dependencies_cycles_and_asynchronous_setup() {
    let source_svc = svc_source();
    let projection_svc = svc_projection();
    let activated = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let src_m = source_svc.clone();
    let act_m = Arc::clone(&activated);
    let missing = define_facet(Facet::new(
        "missing",
        Arc::new(move |env| {
            let _ = env.r#use::<ServiceObject>(&src_m)?;
            let act_cb = Arc::clone(&act_m);
            env.on_activate(Arc::new(move || {
                act_cb.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(futures::future::ready(Ok(())))
            }))
        }),
    ));

    let err_missing = create_facet_host(FacetOptions {
        facets: vec![missing],
        service_sources: None,
        on_error: None,
    })
    .await
    .err()
    .expect("missing dependency should reject");
    assert!(
        err_missing.to_js_string().contains(&format!(
            "Facet missing requires local/{}/singleton, but no facet provides it",
            source_svc.id
        )),
        "unexpected missing error: {}",
        err_missing.to_js_string()
    );
    assert!(!activated.load(std::sync::atomic::Ordering::SeqCst));

    let p1 = projection_svc.clone();
    let s1 = source_svc.clone();
    let first = define_facet(Facet::new(
        "first",
        Arc::new(move |env| {
            let _ = env.r#use::<ServiceObject>(&p1)?;
            env.provide(
                &s1,
                make_read_service(|_ctx| Box::pin(futures::future::ready(Ok(Some(json!("first")))))),
            )
        }),
    ));
    let p2 = projection_svc;
    let s2 = source_svc;
    let second = define_facet(Facet::new(
        "second",
        Arc::new(move |env| {
            let _ = env.r#use::<ServiceObject>(&s2)?;
            env.provide(
                &p2,
                make_read_service(|_ctx| Box::pin(futures::future::ready(Ok(Some(json!("second")))))),
            )
        }),
    ));

    let err_cycle = create_facet_host(FacetOptions {
        facets: vec![first, second],
        service_sources: None,
        on_error: None,
    })
    .await
    .err()
    .expect("dependency cycle should reject");
    assert!(
        err_cycle
            .to_js_string()
            .contains("Facet dependency cycle: first, second")
    );

    // PORT: TS runtime check ("Facet asynchronous setup must be synchronous") is enforced statically
    // in Rust by `Facet::new`'s synchronous `Fn(&FacetEnvironment) -> pi_js::Result<()>` signature.
}

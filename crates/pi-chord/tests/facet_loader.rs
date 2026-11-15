//! Port of packages/chord/test/facet-loader.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_chord::api::DefineServiceOptions;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::handle::{ServiceObject, ServiceValue};
use pi_chord::types::ServiceIdRef;
use pi_chord::{
    Context, Facet, FacetLoader, FacetOptions, LoadedFacets, RemoteServiceBindingOptions, combine_facet_loaders,
    create_facet_host, create_remote_service_binding, create_static_facet_loader, define_facet, define_service,
};
use serde_json::json;

use self::support::helpers::create_loopback_service_transport;

fn make_generation_impl(name: String) -> ServiceObject {
    let mut members = IndexMap::new();
    let name_for_fn = name;
    members.insert(
        "read".to_string(),
        ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
            let value = name_for_fn.clone();
            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                async move { Ok(Some(json!(value))) },
            ))))
        })),
    );
    ServiceObject::from_members(members)
}

async fn invoke_read(read_fn: &ServiceObject, context: &Context) -> pi_js::Result<String> {
    let result = read_fn.apply(
        None,
        &[ServiceValue::Object(ServiceObject::from_context(context.clone()))],
    )?;
    let json = match result {
        ServiceValue::Object(obj) => {
            if let Some(fut) = obj.into_json_future() {
                fut.await?
            } else {
                obj.to_json()
            }
        }
        other => Some(other.to_json()?),
    };
    Ok(json
        .and_then(|v| v.as_str().map(ToOwned::to_owned))
        .expect("read returns string"))
}

async fn read_from_handle(handle: &ServiceObject, context: &Context) -> pi_js::Result<String> {
    let ServiceValue::Object(read_fn) = handle.get_key("read")? else {
        panic!("expected read method");
    };
    invoke_read(&read_fn, context).await
}

#[tokio::test]
async fn combines_loaded_facets_in_loader_order_and_disposes_generations_in_reverse() {
    let first_facet = define_facet(Facet::new("first", Arc::new(|_| Ok(()))));
    let second_facet = define_facet(Facet::new("second", Arc::new(|_| Ok(()))));
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let first = {
        let trace = Arc::clone(&trace);
        let facet = first_facet.clone();
        FacetLoader::new(Arc::new(move || {
            let trace = Arc::clone(&trace);
            let facet = facet.clone();
            Box::pin(async move {
                trace.lock().unwrap().push("load first".to_string());
                let trace_disp = Arc::clone(&trace);
                Ok(LoadedFacets::new(
                    vec![facet],
                    Arc::new(move || {
                        let trace_disp = Arc::clone(&trace_disp);
                        Box::pin(async move {
                            trace_disp.lock().unwrap().push("dispose first".to_string());
                            Ok(())
                        })
                    }),
                ))
            })
        }))
    };

    let second = {
        let trace = Arc::clone(&trace);
        let facet = second_facet.clone();
        FacetLoader::new(Arc::new(move || {
            let trace = Arc::clone(&trace);
            let facet = facet.clone();
            Box::pin(async move {
                trace.lock().unwrap().push("load second".to_string());
                let trace_disp = Arc::clone(&trace);
                Ok(LoadedFacets::new(
                    vec![facet],
                    Arc::new(move || {
                        let trace_disp = Arc::clone(&trace_disp);
                        Box::pin(async move {
                            trace_disp.lock().unwrap().push("dispose second".to_string());
                            Ok(())
                        })
                    }),
                ))
            })
        }))
    };

    let loaded = combine_facet_loaders(vec![first, second]).load().await.unwrap();
    let ids: Vec<&str> = loaded.facets().iter().map(Facet::id).collect();
    assert_eq!(ids, vec!["first", "second"]);
    loaded.dispose().await.unwrap();
    loaded.dispose().await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        vec!["load first", "load second", "dispose second", "dispose first"]
    );
}

#[tokio::test]
async fn cleans_up_loaded_facets_when_a_later_loader_fails() {
    let first_facet = define_facet(Facet::new("first", Arc::new(|_| Ok(()))));
    let dispose_calls = Arc::new(AtomicUsize::new(0));

    let first = {
        let dispose_calls = Arc::clone(&dispose_calls);
        let facet = first_facet.clone();
        FacetLoader::new(Arc::new(move || {
            let dispose_calls = Arc::clone(&dispose_calls);
            let facet = facet.clone();
            Box::pin(async move {
                Ok(LoadedFacets::new(
                    vec![facet],
                    Arc::new(move || {
                        dispose_calls.fetch_add(1, Ordering::SeqCst);
                        Box::pin(futures::future::ready(Ok(())))
                    }),
                ))
            })
        }))
    };

    let second = FacetLoader::new(Arc::new(|| {
        Box::pin(async { Err(pi_js::Error::js("Error", "load failed")) })
    }));

    let err = combine_facet_loaders(vec![first, second]).load().await.err().unwrap();
    assert_eq!(err.message(), "load failed");
    assert_eq!(dispose_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn keeps_local_and_rpc_service_handles_stable_when_their_provider_facet_reloads() {
    let local_gen_value = define_service(
        "test.experimental.local-generation-value",
        Some(DefineServiceOptions { local: Some(true) }),
    )
    .unwrap();
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();

    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let local_value: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));
    let generation = Arc::new(AtomicUsize::new(0));

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let (continue_tx, continue_rx) = tokio::sync::oneshot::channel::<()>();
    let continue_rx = Arc::new(tokio::sync::Mutex::new(Some(continue_rx)));

    let consumer = {
        let local_svc = local_gen_value.clone();
        let trace = Arc::clone(&trace);
        let local_value = Arc::clone(&local_value);
        define_facet(Facet::new(
            "consumer",
            Arc::new(move |env| {
                trace.lock().unwrap().push("setup consumer".to_string());
                let handle = env.r#use::<ServiceObject>(&local_svc)?;
                *local_value.lock().unwrap() = Some(handle.clone());
                let trace_act = Arc::clone(&trace);
                env.on_activate(Arc::new(move || {
                    let trace_act = Arc::clone(&trace_act);
                    let handle = handle.clone();
                    Box::pin(async move {
                        let val = read_from_handle(&handle, &BACKGROUND_CONTEXT).await?;
                        trace_act.lock().unwrap().push(format!("activate consumer:{val}"));
                        Ok(())
                    })
                }))?;
                let trace_deact = Arc::clone(&trace);
                env.on_deactivate(Arc::new(move || {
                    trace_deact.lock().unwrap().push("deactivate consumer".to_string());
                    Box::pin(futures::future::ready(Ok(())))
                }))?;
                Ok(())
            }),
        ))
    };

    let loader = {
        let local_svc = local_gen_value.clone();
        let remote_svc = remote_gen_value.clone();
        let trace = Arc::clone(&trace);
        let generation = Arc::clone(&generation);
        let started_tx = Arc::clone(&started_tx);
        let continue_rx = Arc::clone(&continue_rx);
        FacetLoader::new(Arc::new(move || {
            let local_svc = local_svc.clone();
            let remote_svc = remote_svc.clone();
            let trace = Arc::clone(&trace);
            let generation = Arc::clone(&generation);
            let started_tx = Arc::clone(&started_tx);
            let continue_rx = Arc::clone(&continue_rx);
            Box::pin(async move {
                let name = if generation.fetch_add(1, Ordering::SeqCst) == 0 {
                    "A"
                } else {
                    "B"
                };
                trace.lock().unwrap().push(format!("load {name}"));
                let name_s = name.to_string();
                let trace_setup = Arc::clone(&trace);
                let provider_facet = define_facet(Facet::new(
                    "provider",
                    Arc::new(move |env| {
                        trace_setup.lock().unwrap().push(format!("setup provider {name_s}"));
                        let implementation = make_generation_impl(name_s.clone());
                        env.provide(&local_svc, implementation.clone())?;
                        env.provide(&remote_svc, implementation)?;
                        let trace_act = Arc::clone(&trace_setup);
                        let name_act = name_s.clone();
                        let started_tx = Arc::clone(&started_tx);
                        let continue_rx = Arc::clone(&continue_rx);
                        env.on_activate(Arc::new(move || {
                            let trace_act = Arc::clone(&trace_act);
                            let name_act = name_act.clone();
                            let started_tx = Arc::clone(&started_tx);
                            let continue_rx = Arc::clone(&continue_rx);
                            Box::pin(async move {
                                trace_act.lock().unwrap().push(format!("activate provider {name_act}"));
                                if name_act == "B" {
                                    if let Some(tx) = started_tx.lock().unwrap().take() {
                                        let _ = tx.send(());
                                    }
                                    let rx = continue_rx.lock().await.take();
                                    if let Some(rx) = rx {
                                        let _ = rx.await;
                                    }
                                }
                                Ok(())
                            })
                        }))?;
                        let trace_deact = Arc::clone(&trace_setup);
                        let name_deact = name_s.clone();
                        env.on_deactivate(Arc::new(move || {
                            trace_deact
                                .lock()
                                .unwrap()
                                .push(format!("deactivate provider {name_deact}"));
                            Box::pin(futures::future::ready(Ok(())))
                        }))?;
                        Ok(())
                    }),
                ));
                let trace_unload = Arc::clone(&trace);
                let name_unload = name.to_string();
                Ok(LoadedFacets::new(
                    vec![provider_facet],
                    Arc::new(move || {
                        trace_unload.lock().unwrap().push(format!("unload {name_unload}"));
                        Box::pin(futures::future::ready(Ok(())))
                    }),
                ))
            })
        }))
    };

    let loaded_a = loader.load().await.unwrap();
    let mut initial_facets = vec![consumer];
    initial_facets.extend(loaded_a.facets().iter().cloned());
    let host = create_facet_host(FacetOptions {
        facets: initial_facets,
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let original_local_handle = local_value.lock().unwrap().clone().unwrap();
    let ServiceValue::Object(local_read) = original_local_handle.get_key("read").unwrap() else {
        panic!("expected local read method");
    };
    let remote_services = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: remote_gen_value.id.clone(),
        }],
        transport: create_loopback_service_transport(&host.services()),
        bound: None,
        on_error: None,
        assert_access: None,
    })
    .unwrap();
    let original_remote_handle = remote_services.r#use::<ServiceObject>(&remote_gen_value).unwrap();
    let ServiceValue::Object(remote_read) = original_remote_handle.get_key("read").unwrap() else {
        panic!("expected remote read method");
    };
    remote_services.ready(BACKGROUND_CONTEXT.clone()).await.unwrap();
    assert_eq!(invoke_read(&local_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");
    assert_eq!(invoke_read(&remote_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");

    let invalid_reload_err = host
        .reload(vec![define_facet(Facet::new(
            "provider",
            Arc::new({
                let local_svc = local_gen_value.clone();
                move |env| env.provide(&local_svc, make_generation_impl("invalid".to_string()))
            }),
        ))])
        .await
        .unwrap_err();
    assert!(
        invalid_reload_err
            .message()
            .contains("Reloaded facet provider must preserve its service requirements and provisions"),
        "got: {invalid_reload_err}"
    );
    assert_eq!(invoke_read(&local_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");
    assert_eq!(invoke_read(&remote_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");

    let loaded_b = loader.load().await.unwrap();
    let host_for_reload = host.clone();
    let reload_facets = loaded_b.facets().to_vec();
    let reload = tokio::spawn(async move { host_for_reload.reload(reload_facets).await });
    started_rx.await.unwrap();
    assert_eq!(invoke_read(&local_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");
    assert_eq!(invoke_read(&remote_read, &BACKGROUND_CONTEXT).await.unwrap(), "A");
    continue_tx.send(()).unwrap();
    reload.await.unwrap().unwrap();
    loaded_a.dispose().await.unwrap();

    assert!(ServiceObject::ptr_eq(
        local_value.lock().unwrap().as_ref().unwrap(),
        &original_local_handle
    ));
    assert!(ServiceObject::ptr_eq(
        &remote_services.r#use::<ServiceObject>(&remote_gen_value).unwrap(),
        &original_remote_handle
    ));
    assert_eq!(invoke_read(&local_read, &BACKGROUND_CONTEXT).await.unwrap(), "B");
    assert_eq!(invoke_read(&remote_read, &BACKGROUND_CONTEXT).await.unwrap(), "B");

    remote_services.dispose(BACKGROUND_CONTEXT.clone()).await.unwrap();
    host.dispose().await.unwrap();
    loaded_b.dispose().await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        vec![
            "load A",
            "setup consumer",
            "setup provider A",
            "activate provider A",
            "activate consumer:A",
            "load B",
            "setup provider B",
            "activate provider B",
            "deactivate provider A",
            "unload A",
            "deactivate consumer",
            "deactivate provider B",
            "unload B",
        ]
    );
}

#[tokio::test]
async fn rejects_remote_singleton_member_shape_changes_before_reload_cutover() {
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();
    let retained: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));
    let provider_disposed = Arc::new(AtomicBool::new(false));

    let consumer = define_facet(Facet::new(
        "shape-consumer",
        Arc::new({
            let svc = remote_gen_value.clone();
            let retained = Arc::clone(&retained);
            move |env| {
                *retained.lock().unwrap() = Some(env.r#use::<ServiceObject>(&svc)?);
                Ok(())
            }
        }),
    ));

    let provider = define_facet(Facet::new(
        "shape-provider",
        Arc::new({
            let svc = remote_gen_value.clone();
            let disposed = Arc::clone(&provider_disposed);
            move |env| {
                env.provide(&svc, make_generation_impl("A".to_string()))?;
                let disposed = Arc::clone(&disposed);
                env.on_deactivate(Arc::new(move || {
                    disposed.store(true, Ordering::Release);
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }
        }),
    ));

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, provider],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let err = host
        .reload(vec![define_facet(Facet::new(
            "shape-provider",
            Arc::new({
                let svc = remote_gen_value.clone();
                move |env| {
                    let mut members = IndexMap::new();
                    members.insert(
                        "renamed".to_string(),
                        ServiceValue::Object(ServiceObject::from_fn(|_this, _args| {
                            Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(async {
                                Ok(Some(json!("B")))
                            }))))
                        })),
                    );
                    env.provide(&svc, ServiceObject::from_members(members))
                }
            }),
        ))])
        .await
        .unwrap_err();
    assert!(
        err.message().contains("replacement must preserve its member shape"),
        "got: {err}"
    );
    assert!(!provider_disposed.load(Ordering::Acquire));
    let retained_handle = retained.lock().unwrap().clone().unwrap();
    assert_eq!(
        read_from_handle(&retained_handle, &BACKGROUND_CONTEXT).await.unwrap(),
        "A"
    );

    host.dispose().await.unwrap();
    assert!(provider_disposed.load(Ordering::Acquire));
}

#[tokio::test]
async fn terminates_the_host_when_old_cleanup_fails_after_cutover() {
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();
    let make_provider = |name: &'static str, fail_cleanup: bool| {
        let svc = remote_gen_value.clone();
        define_facet(Facet::new(
            "cleanup-provider",
            Arc::new(move |env| {
                env.provide(&svc, make_generation_impl(name.to_string()))?;
                env.on_deactivate(Arc::new(move || {
                    if fail_cleanup {
                        Box::pin(futures::future::ready(Err(pi_js::Error::js("Error", "cleanup failed"))))
                    } else {
                        Box::pin(futures::future::ready(Ok(())))
                    }
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![make_provider("A", true)],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();

    let err = host.reload(vec![make_provider("B", false)]).await.unwrap_err();
    assert!(
        err.message().contains("Facet reload failed after cutover"),
        "got: {err}"
    );
    let dead_err = host.reload(vec![]).await.unwrap_err();
    assert!(
        dead_err.message().contains("Facet host cannot reload while dead"),
        "got: {dead_err}"
    );
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn cleans_failed_candidate_activation_in_reverse_dependency_order() {
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let make_provider = |name: &'static str| {
        let svc = remote_gen_value.clone();
        let trace = Arc::clone(&trace);
        define_facet(Facet::new(
            "ordered-provider",
            Arc::new(move |env| {
                env.provide(&svc, make_generation_impl(name.to_string()))?;
                let trace_act = Arc::clone(&trace);
                env.on_activate(Arc::new(move || {
                    trace_act.lock().unwrap().push(format!("activate provider {name}"));
                    Box::pin(futures::future::ready(Ok(())))
                }))?;
                let trace_deact = Arc::clone(&trace);
                env.on_deactivate(Arc::new(move || {
                    trace_deact.lock().unwrap().push(format!("deactivate provider {name}"));
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };
    let make_consumer = |name: &'static str, fail: bool| {
        let svc = remote_gen_value.clone();
        let trace = Arc::clone(&trace);
        define_facet(Facet::new(
            "ordered-consumer",
            Arc::new(move |env| {
                let _ = env.r#use::<ServiceObject>(&svc)?;
                let trace_act = Arc::clone(&trace);
                env.on_activate(Arc::new(move || {
                    trace_act.lock().unwrap().push(format!("activate consumer {name}"));
                    if fail {
                        Box::pin(futures::future::ready(Err(pi_js::Error::js(
                            "Error",
                            "consumer activation failed",
                        ))))
                    } else {
                        Box::pin(futures::future::ready(Ok(())))
                    }
                }))?;
                let trace_deact = Arc::clone(&trace);
                env.on_deactivate(Arc::new(move || {
                    trace_deact.lock().unwrap().push(format!("deactivate consumer {name}"));
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![make_consumer("A", false), make_provider("A")],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();
    trace.lock().unwrap().clear();

    let err = host
        .reload(vec![make_consumer("B", true), make_provider("B")])
        .await
        .unwrap_err();
    assert_eq!(err.message(), "consumer activation failed");
    assert_eq!(
        *trace.lock().unwrap(),
        vec![
            "activate provider B",
            "activate consumer B",
            "deactivate consumer B",
            "deactivate provider B",
        ]
    );
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn terminates_the_host_when_replacement_publication_fails_after_cutover() {
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();
    let make_provider = |name: &'static str| {
        let svc = remote_gen_value.clone();
        define_facet(Facet::new(
            "publication-provider",
            Arc::new(move |env| env.provide(&svc, make_generation_impl(name.to_string()))),
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
        .subscribe_fallible(&remote_gen_value.id, pi_chord::ServiceMode::Singleton, |_, _| {
            Err(pi_js::Error::js("Error", "publication failed"))
        })
        .unwrap();
    subscription.activate().unwrap();

    let err = host.reload(vec![make_provider("B")]).await.unwrap_err();
    assert!(
        err.message().contains("Facet reload failed after cutover"),
        "got: {err}"
    );
    let dead_err = host.reload(vec![]).await.unwrap_err();
    assert!(
        dead_err.message().contains("Facet host cannot reload while dead"),
        "got: {dead_err}"
    );
    host.dispose().await.unwrap();
}

#[tokio::test]
async fn keeps_the_old_generation_active_when_replacement_activation_fails_before_cutover() {
    let remote_gen_value = define_service("test.experimental.remote-generation-value", None).unwrap();
    let trace: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let retained: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));

    let consumer = define_facet(Facet::new(
        "terminal-consumer",
        Arc::new({
            let svc = remote_gen_value.clone();
            let retained = Arc::clone(&retained);
            let trace = Arc::clone(&trace);
            move |env| {
                *retained.lock().unwrap() = Some(env.r#use::<ServiceObject>(&svc)?);
                let trace = Arc::clone(&trace);
                env.on_deactivate(Arc::new(move || {
                    trace.lock().unwrap().push("deactivate consumer".to_string());
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }
        }),
    ));
    let make_provider = |name: &'static str, fail: bool| {
        let svc = remote_gen_value.clone();
        let trace = Arc::clone(&trace);
        define_facet(Facet::new(
            "terminal-provider",
            Arc::new(move |env| {
                env.provide(&svc, make_generation_impl(name.to_string()))?;
                let trace_act = Arc::clone(&trace);
                env.on_activate(Arc::new(move || {
                    trace_act.lock().unwrap().push(format!("activate {name}"));
                    if fail {
                        Box::pin(futures::future::ready(Err(pi_js::Error::js(
                            "Error",
                            "replacement activation failed",
                        ))))
                    } else {
                        Box::pin(futures::future::ready(Ok(())))
                    }
                }))?;
                let trace_deact = Arc::clone(&trace);
                env.on_deactivate(Arc::new(move || {
                    trace_deact.lock().unwrap().push(format!("deactivate {name}"));
                    Box::pin(futures::future::ready(Ok(())))
                }))
            }),
        ))
    };

    let host = create_facet_host(FacetOptions {
        facets: vec![consumer, make_provider("A", false)],
        service_sources: None,
        on_error: None,
    })
    .await
    .unwrap();
    let retained_handle = retained.lock().unwrap().clone().unwrap();
    assert_eq!(
        read_from_handle(&retained_handle, &BACKGROUND_CONTEXT).await.unwrap(),
        "A"
    );

    let err = host.reload(vec![make_provider("B", true)]).await.unwrap_err();
    assert_eq!(err.message(), "replacement activation failed");
    assert_eq!(*trace.lock().unwrap(), vec!["activate A", "activate B", "deactivate B"]);
    assert_eq!(
        read_from_handle(&retained_handle, &BACKGROUND_CONTEXT).await.unwrap(),
        "A"
    );
    host.reload(vec![]).await.unwrap();
    host.dispose().await.unwrap();
    assert_eq!(
        *trace.lock().unwrap(),
        vec![
            "activate A",
            "activate B",
            "deactivate B",
            "deactivate consumer",
            "deactivate A",
        ]
    );
}

#[tokio::test]
async fn creates_a_reusable_static_loader() {
    let first_facet = define_facet(Facet::new("first", Arc::new(|_| Ok(()))));
    let loader = create_static_facet_loader(vec![first_facet.clone()]);
    let first = loader.load().await.unwrap();
    let second = loader.load().await.unwrap();
    assert_eq!(
        first.facets().iter().map(Facet::id).collect::<Vec<_>>(),
        vec![first_facet.id()]
    );
    assert_eq!(
        second.facets().iter().map(Facet::id).collect::<Vec<_>>(),
        vec![first_facet.id()]
    );
    let (r1, r2) = futures::join!(first.dispose(), second.dispose());
    r1.unwrap();
    r2.unwrap();
}

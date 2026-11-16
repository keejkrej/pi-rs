//! Port of packages/durable/test/chord-guide.test.ts
//!
//! The code of docs/pico-v5-chord-usage.md, compiled and run against a real Harness. Keep the two in sync: the
//! guide's blocks are copied here apart from Rust formatting and the output sinks, which record instead of printing.

mod support;

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::Op;
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::loopback::create_loopback_service_transport;
use pi_chord::types::{ReplicatedStateDelivery, ReplicatedStateDeliveryKind, ServiceIdRef};
use pi_chord::{
    Context, Facet, FacetHost, FacetOptions, JsonValue, RemoteServiceBindingOptions, RemoteServiceTransport, Service,
    create_facet_host, create_remote_service_binding, define_facet, define_service,
};
use pi_durable::documents::{
    ConversationDocFamilyToken, DocDefinitionInput, DocFamilyDefinitionInput, DocumentState, SessionDocToken,
    TaskDocToken, define_conversation_doc_family, define_session_doc, define_task_doc,
};
use pi_durable::tasks::{AnyTask, TaskDefinition, TaskRuntimeHandle, TypedPhaseHandler, define_task};
use pi_durable::types::{ConversationId, ConversationOwnership, LatestFork, TaskId, TaskOptions, TaskOwnership};
use pi_durable::{Harness, MemoryStorage, Session};
use pi_js::Unsubscribe;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::session_support::context;
use support::task_support::{OpenTasksOptions, completed, deferred, eventually_sync, open_tasks};

type LogRecord = (String, i64, JsonValue);

fn wrap_document_state<T: Send + Sync + 'static>(state: &DocumentState<T>) -> ServiceObject {
    struct AttachedDocTarget(pi_chord::AttachedReplicatedState<JsonValue>);

    impl ServiceTarget for AttachedDocTarget {
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
                    let unsub = state.subscribe(Arc::new(move |val, ctx, delivery: ReplicatedStateDelivery| {
                        let cb = callback.clone();
                        Box::pin(async move {
                            let kind_str = match delivery.kind {
                                ReplicatedStateDeliveryKind::Hydrate => "hydrate",
                                ReplicatedStateDeliveryKind::Update => "update",
                            };
                            let del_json = serde_json::json!({
                                "kind": kind_str,
                                "sequence": delivery.sequence,
                            });
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
                        if let Some(u) = cell.lock().unwrap_or_else(|e| e.into_inner()).take() {
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

    let internals = state.internals().expect("document state internals must be registered");
    ServiceObject::from_state(AttachedDocTarget(state.as_raw().clone()), internals)
}

fn subscribe_state_with_delivery(
    service: &ServiceObject,
    member: &str,
    on_event: impl Fn(JsonValue, ReplicatedStateDelivery) + Send + Sync + 'static,
) -> pi_js::Result<ServiceObject> {
    let ServiceValue::Object(state_obj) = service.get_key(member)? else {
        panic!("expected state member {member}");
    };
    let ServiceValue::Object(sub_fn) = state_obj.get_key("subscribe")? else {
        panic!("expected subscribe function");
    };
    let cb = Arc::new(on_event);
    let listener = ServiceObject::from_fn(move |_this, args| {
        let value = args
            .first()
            .map(|v| v.to_json())
            .transpose()?
            .unwrap_or(JsonValue::Null);
        let delivery = match args.get(2) {
            Some(v) => {
                let raw = v.to_json()?;
                serde_json::from_value::<ReplicatedStateDelivery>(raw)
                    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?
            }
            None => ReplicatedStateDelivery {
                kind: ReplicatedStateDeliveryKind::Hydrate,
                sequence: 0,
            },
        };
        cb(value, delivery);
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
    promise
        .into_json_future()
        .expect("method return must return promise")
        .await
}

fn stop_unsubscribe_obj(unsub: &ServiceObject) -> pi_js::Result<()> {
    let _ = unsub.apply(None, &[])?;
    Ok(())
}

// ─── 1. A Session-wide canvas ────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Point {
    x: i64,
    y: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stroke {
    color: String,
    points: Vec<Point>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CanvasState {
    strokes: Vec<Stroke>,
}

fn canvas_doc() -> SessionDocToken<CanvasState> {
    define_session_doc(DocDefinitionInput {
        kind: "app.canvas",
        version: 1,
        initial: CanvasState::default,
        migrate: None,
        // Store a complete base after at most 99 replayed deltas.
        checkpoint_when: Some(|_value, _ops, info| info.deltas_since_base >= 99),
    })
    .expect("valid CanvasDoc")
}

fn svc_canvas() -> Service {
    define_service("app.canvas", None).expect("valid Canvas service")
}

async fn create_canvas_facet(
    session: &Session,
    doc: SessionDocToken<CanvasState>,
    canvas_svc: &Service,
    call_context: Context,
) -> pi_js::Result<Facet> {
    // Creation is explicit; observation never writes.
    let doc_init = doc.clone();
    session
        .commit(
            move |tx| async move {
                let _ = tx.doc_session(&doc_init).await?;
                Ok(())
            },
            call_context.clone(),
        )
        .await?;
    let state = session
        .document_state_session(&doc, call_context)
        .await?
        .ok_or_else(|| pi_js::Error::msg("canvas was retired during setup"))?;
    let svc = canvas_svc.clone();
    let session_c = session.clone();
    Ok(define_facet(Facet::new(
        "app.canvas/session",
        Arc::new(move |env| {
            let state_own = state.clone();
            env.own(Arc::new(move || {
                let state = state_own.clone();
                Box::pin(async move { state.dispose() })
            }))?;

            let mut members = IndexMap::new();
            members.insert("state".to_owned(), ServiceValue::Object(wrap_document_state(&state)));
            let session_add = session_c.clone();
            let doc_add = doc.clone();
            members.insert(
                "addStroke".to_owned(),
                ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                    let raw_stroke = args
                        .first()
                        .map(|v| v.to_json())
                        .transpose()?
                        .ok_or_else(|| pi_js::Error::js("TypeError", "missing stroke argument"))?;
                    let stroke: Stroke = serde_json::from_value(raw_stroke)
                        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                    let ctx = args
                        .get(1)
                        .and_then(|v| match v {
                            ServiceValue::Object(o) => o.as_context(),
                            _ => None,
                        })
                        .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
                    let session_commit = session_add.clone();
                    let doc_commit = doc_add.clone();
                    Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                        async move {
                            session_commit
                                .commit(
                                    move |tx| async move {
                                        tx.doc_session(&doc_commit)
                                            .await?
                                            .update(|draft| draft.strokes.push(stroke))?;
                                        Ok(())
                                    },
                                    ctx,
                                )
                                .await?;
                            Ok(None)
                        },
                    ))))
                })),
            );
            env.provide(&svc, ServiceObject::from_members(members))
        }),
    )))
}

fn create_canvas_consumer(canvas_svc: &Service, logged: Arc<Mutex<Vec<LogRecord>>>) -> Facet {
    let svc = canvas_svc.clone();
    define_facet(Facet::new(
        "app.canvas/consumer",
        Arc::new(move |env| {
            let canvas = env.r#use::<ServiceObject>(&svc)?; // Declare now; access only after activation.
            let stop_slot: Arc<Mutex<Option<ServiceObject>>> = Arc::new(Mutex::new(None));
            let stop_own = Arc::clone(&stop_slot);
            env.own(
                Arc::new(move || {
                    if let Some(unsub) = stop_own.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        stop_unsubscribe_obj(&unsub)?;
                    }
                    Ok(())
                })
                .into_async_disposal(),
            )?;
            let logged_cb = Arc::clone(&logged);
            env.on_activate(Arc::new(move || {
                let stop = subscribe_state_with_delivery(&canvas, "state", {
                    let logged = Arc::clone(&logged_cb);
                    move |value, delivery| {
                        let count = value
                            .as_object()
                            .and_then(|o| o.get("strokes"))
                            .and_then(|s| s.as_array())
                            .map(|arr| JsonValue::from(arr.len() as i64))
                            .unwrap_or_else(|| JsonValue::String("retired".to_owned()));
                        let kind = match delivery.kind {
                            ReplicatedStateDeliveryKind::Hydrate => "hydrate",
                            ReplicatedStateDeliveryKind::Update => "update",
                        };
                        logged.lock().unwrap_or_else(|e| e.into_inner()).push((
                            kind.to_owned(),
                            delivery.sequence,
                            count,
                        ));
                    }
                })?;
                *stop_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop);
                Box::pin(async { Ok(()) })
            }))
        }),
    ))
}

trait IntoAsyncDisposal {
    fn into_async_disposal(self) -> Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
}

impl<F> IntoAsyncDisposal for F
where
    F: Fn() -> pi_js::Result<()> + Send + Sync + 'static,
{
    fn into_async_disposal(self) -> Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync> {
        Arc::new(move || {
            let res = (self)();
            Box::pin(async move { res })
        })
    }
}

async fn run_canvas_example(
    session: &Session,
    doc: SessionDocToken<CanvasState>,
    canvas_svc: &Service,
    logged: Arc<Mutex<Vec<LogRecord>>>,
) -> pi_js::Result<()> {
    let context = BACKGROUND_CONTEXT.clone();
    let provider = create_canvas_facet(session, doc, canvas_svc, context.clone()).await?;
    let consumer = create_canvas_consumer(canvas_svc, logged);
    let host = create_facet_host(FacetOptions {
        facets: vec![provider, consumer],
        service_sources: None,
        on_error: None,
    })
    .await?;
    let canvas_obj = host.services().use_::<ServiceObject>(canvas_svc)?;
    let stroke = serde_json::json!({
        "color": "black",
        "points": [
            { "x": 10, "y": 20 },
            { "x": 30, "y": 40 }
        ]
    });
    let res = invoke_method(
        &canvas_obj,
        "addStroke",
        vec![
            ServiceValue::from_json(&stroke),
            ServiceValue::Object(ServiceObject::from_context(context)),
        ],
    )
    .await;
    let dispose_res = host.dispose().await;
    res?;
    dispose_res?;
    Ok(())
}

async fn connect_canvas(
    canvas_svc: &Service,
    transport: RemoteServiceTransport,
    call_context: Context,
    logged: Arc<Mutex<Vec<LogRecord>>>,
) -> pi_js::Result<Box<dyn FnOnce() -> pi_js::BoxFuture<pi_js::Result<()>> + Send>> {
    let services = create_remote_service_binding(RemoteServiceBindingOptions {
        services: vec![ServiceIdRef {
            id: canvas_svc.id.clone(),
        }],
        transport,
        bound: None,
        on_error: None,
        assert_access: None,
    })?;
    let canvas = services.r#use(canvas_svc)?;
    let stop = subscribe_state_with_delivery(&canvas, "state", move |value, delivery| {
        let strokes = value
            .as_object()
            .and_then(|o| o.get("strokes"))
            .cloned()
            .unwrap_or_else(|| JsonValue::String("retired".to_owned()));
        let kind = match delivery.kind {
            ReplicatedStateDeliveryKind::Hydrate => "hydrate",
            ReplicatedStateDeliveryKind::Update => "update",
        };
        logged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((kind.to_owned(), delivery.sequence, strokes));
    })?;
    if let Err(err) = services.ready(call_context).await {
        let _ = stop_unsubscribe_obj(&stop);
        let _ = services.dispose(BACKGROUND_CONTEXT.clone()).await;
        return Err(err);
    }
    Ok(Box::new(move || {
        Box::pin(async move {
            stop_unsubscribe_obj(&stop)?;
            services.dispose(BACKGROUND_CONTEXT.clone()).await
        })
    }))
}

async fn shutdown(
    host: &FacetHost,
    detach_clients: impl FnOnce() -> pi_js::BoxFuture<pi_js::Result<()>>,
    harness: &Harness,
    order: &Mutex<Vec<String>>,
    call_context: Context,
) -> pi_js::Result<()> {
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("detach start".to_owned());
    detach_clients().await?;
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("detach end".to_owned());
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("dispose start".to_owned());
    host.dispose().await?;
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("dispose end".to_owned());
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("close start".to_owned());
    harness.close(call_context).await?;
    order
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push("close end".to_owned());
    Ok(())
}

// ─── 2. Conversation-scoped diff reviews ─────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewInput {
    path: String,
    patch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewComment {
    id: String,
    line: i64,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReviewState {
    path: String,
    patch: String,
    comments: Vec<ReviewComment>,
}

fn review_doc() -> ConversationDocFamilyToken<ReviewState, ReviewInput> {
    define_conversation_doc_family(
        LatestFork::Current,
        DocFamilyDefinitionInput {
            kind: "app.diff-review",
            version: 1,
            initial: |seed: ReviewInput| ReviewState {
                path: seed.path,
                patch: seed.patch,
                comments: Vec::new(),
            },
            migrate: None,
            checkpoint_when: Some(|_value, _ops, info| info.deltas_since_base >= 49),
        },
    )
    .expect("valid ReviewDoc")
}

fn svc_diff_reviews() -> Service {
    define_service("app.diff-reviews", None).expect("valid DiffReviews service")
}

#[derive(Clone)]
struct ReviewSpec {
    key: String,
    seed: ReviewInput,
}

fn review_facet(
    session: &Session,
    doc: ConversationDocFamilyToken<ReviewState, ReviewInput>,
    diff_reviews_svc: &Service,
    conversation_id: ConversationId,
    reviews: Vec<ReviewSpec>,
    call_context: Context,
) -> Facet {
    let session_c = session.clone();
    let svc = diff_reviews_svc.clone();
    define_facet(Facet::new(
        "app.diff-reviews/session",
        Arc::new(move |env| {
            let instances = env.provide_many::<ServiceObject>(&svc)?;
            let owned_states: Arc<Mutex<Vec<DocumentState<ReviewState>>>> = Arc::new(Mutex::new(Vec::new()));
            let owned_stops: Arc<Mutex<Vec<Unsubscribe>>> = Arc::new(Mutex::new(Vec::new()));
            let states_own = Arc::clone(&owned_states);
            let stops_own = Arc::clone(&owned_stops);
            env.own(
                Arc::new(move || {
                    let stops: Vec<Unsubscribe> = {
                        let mut guard = stops_own.lock().unwrap_or_else(|e| e.into_inner());
                        std::mem::take(&mut *guard)
                    };
                    for u in stops {
                        u.call();
                    }
                    let states: Vec<DocumentState<ReviewState>> = {
                        let mut guard = states_own.lock().unwrap_or_else(|e| e.into_inner());
                        std::mem::take(&mut *guard)
                    };
                    for st in states {
                        st.dispose()?;
                    }
                    Ok(())
                })
                .into_async_disposal(),
            )?;

            let session_act = session_c.clone();
            let doc_act = doc.clone();
            let reviews_act = reviews.clone();
            let ctx_act = call_context.clone();
            let states_act = Arc::clone(&owned_states);
            let stops_act = Arc::clone(&owned_stops);
            env.on_activate(Arc::new(move || {
                let session = session_act.clone();
                let doc = doc_act.clone();
                let reviews = reviews_act.clone();
                let ctx = ctx_act.clone();
                let instances = instances.clone();
                let states_act = Arc::clone(&states_act);
                let stops_act = Arc::clone(&stops_act);
                Box::pin(async move {
                    for review in reviews {
                        let doc_init = doc.clone();
                        let key_init = review.key.clone();
                        let seed_init = review.seed.clone();
                        session
                            .commit(
                                move |tx| async move {
                                    let _ = tx
                                        .doc_conversation_family(&doc_init, conversation_id, &key_init, seed_init)
                                        .await?;
                                    Ok(())
                                },
                                ctx.clone(),
                            )
                            .await?;
                        let state = session
                            .document_state_conversation_family(&doc, conversation_id, &review.key, ctx.clone())
                            .await?
                            .ok_or_else(|| pi_js::Error::msg("review was retired during setup"))?;
                        states_act.lock().unwrap_or_else(|e| e.into_inner()).push(state.clone());

                        let mut members = IndexMap::new();
                        members.insert("state".to_owned(), ServiceValue::Object(wrap_document_state(&state)));
                        let key_id = review.key.clone();
                        members.insert(
                            "identity".to_owned(),
                            ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
                                let payload = serde_json::json!({
                                    "conversationId": conversation_id.as_i64(),
                                    "key": key_id,
                                });
                                Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                                    futures::future::ready(Ok(Some(payload))),
                                ))))
                            })),
                        );
                        let session_comment = session.clone();
                        let doc_comment = doc.clone();
                        let key_comment = review.key.clone();
                        let seed_comment = review.seed.clone();
                        members.insert(
                            "addComment".to_owned(),
                            ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                                let raw_comment = args
                                    .first()
                                    .map(|v| v.to_json())
                                    .transpose()?
                                    .ok_or_else(|| pi_js::Error::js("TypeError", "missing comment"))?;
                                let comment: ReviewComment = serde_json::from_value(raw_comment)
                                    .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                                let call_ctx = args
                                    .get(1)
                                    .and_then(|v| match v {
                                        ServiceValue::Object(o) => o.as_context(),
                                        _ => None,
                                    })
                                    .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
                                let session_c = session_comment.clone();
                                let doc_c = doc_comment.clone();
                                let key_c = key_comment.clone();
                                let seed_c = seed_comment.clone();
                                Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                                    async move {
                                        session_c
                                            .commit(
                                                move |tx| async move {
                                                    tx.doc_conversation_family(&doc_c, conversation_id, &key_c, seed_c)
                                                        .await?
                                                        .update(|draft| draft.comments.push(comment))?;
                                                    Ok(())
                                                },
                                                call_ctx,
                                            )
                                            .await?;
                                        Ok(None)
                                    },
                                ))))
                            })),
                        );

                        // Chord instance keys route services; they are not numeric document incarnation IDs.
                        let instance_key = pi_js::json::stringify(&(conversation_id.as_i64(), review.key.as_str()));
                        let unsub = instances.spawn(&instance_key, ServiceObject::from_members(members));
                        stops_act.lock().unwrap_or_else(|e| e.into_inner()).push(unsub);
                    }
                    Ok(())
                })
            }))
        }),
    ))
}

// ─── 3. Task-scoped output and a tool/task watch ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct JobInput {
    command: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct JobState {
    phase: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct JobOutput {
    stdout: String,
    chunks: i64,
}

fn job_output_doc() -> TaskDocToken<JobOutput> {
    define_task_doc(DocDefinitionInput {
        kind: "app.job-output",
        version: 1,
        initial: JobOutput::default,
        migrate: None,
        checkpoint_when: Some(|_value, _ops, info| info.deltas_since_base >= 99),
    })
    .expect("valid JobOutputDoc")
}

async fn append_job_output(
    doc: &TaskDocToken<JobOutput>,
    runtime: &TaskRuntimeHandle<JobInput, JobState, (), ()>,
    chunk: &str,
    call_context: Context,
) -> pi_js::Result<()> {
    let tid = runtime.task_id()?.erased();
    let doc_c = doc.clone();
    let chunk = chunk.to_owned();
    // Read process output outside this callback. The runtime gates the live task.
    runtime
        .commit(
            move |tx, _current| {
                let doc_c = doc_c.clone();
                let chunk = chunk.clone();
                Box::pin(async move {
                    tx.doc_task(&doc_c, tid).await?.update(|draft| {
                        let combined = format!("{}{chunk}", draft.stdout);
                        let tail_start = combined.len().saturating_sub(50_000);
                        let safe_start = combined.ceil_char_boundary(tail_start);
                        draft.stdout = combined[safe_start..].to_owned();
                        draft.chunks += 1;
                    })?;
                    Ok(None)
                })
            },
            call_context,
        )
        .await
}

async fn observe_job(
    session: &Session,
    doc: &TaskDocToken<JobOutput>,
    producer_task_id: TaskId,
    finished: impl std::future::Future<Output = ()>,
    call_context: Context,
    logged: Arc<Mutex<Vec<String>>>,
    sent: Arc<Mutex<Vec<(Option<JobOutput>, Vec<Op>)>>>,
    rendered: Arc<Mutex<Vec<String>>>,
) -> pi_js::Result<()> {
    let Some(watch) = session.watch_doc_task(doc, producer_task_id, call_context).await? else {
        return Ok(());
    };
    let baseline = watch.value();
    logged.lock().unwrap_or_else(|e| e.into_inner()).push(match &baseline {
        None => "retired".to_owned(),
        Some(v) => v.stdout.clone(),
    });
    watch.start(Arc::new(move |value, ops, _ctx| {
        let sent = Arc::clone(&sent);
        let rendered = Arc::clone(&rendered);
        Box::pin(async move {
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((value.clone(), ops));
            rendered.lock().unwrap_or_else(|e| e.into_inner()).push(match value {
                None => "retired".to_owned(),
                Some(v) => v.stdout,
            });
            Ok(())
        })
    }))?; // Serialized callbacks never overlap.
    finished.await; // Caller-supplied observation lifetime; outside any commit.
    let _ = watch.stop().await; // Idempotent; prevents another callback from starting.
    Ok(())
}

/// In-process transport to a host's service provider; a real client puts a socket between the two.
fn in_process(host: &FacetHost) -> RemoteServiceTransport {
    create_loopback_service_transport(&host.services())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn runs_the_canvas_facet_host_a_remote_client_withdrawal_and_detach_before_harness_close() {
    let doc = canvas_doc();
    let canvas_svc = svc_canvas();
    let logged: Arc<Mutex<Vec<LogRecord>>> = Arc::new(Mutex::new(Vec::new()));

    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        Vec::new(),
        OpenTasksOptions {
            registry: None,
            now: None,
            settings: None,
        },
    )
    .await
    .expect("open tasks");
    let harness = opened.harness;

    run_canvas_example(&harness, doc.clone(), &canvas_svc, Arc::clone(&logged))
        .await
        .expect("run canvas example");
    assert_eq!(
        *logged.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            ("hydrate".to_owned(), 0, JsonValue::from(0)),
            ("update".to_owned(), 1, JsonValue::from(1)),
        ]
    );
    let snap = harness
        .snapshot_session(&doc, context())
        .await
        .expect("snapshot")
        .expect("canvas present");
    assert_eq!(snap.strokes.len(), 1);

    // A worker installs the canvas facet again; a late remote client hydrates the stroke, then follows updates.
    logged.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let facet = create_canvas_facet(&harness, doc.clone(), &canvas_svc, context())
        .await
        .expect("canvas facet");
    let host = create_facet_host(FacetOptions {
        facets: vec![facet],
        service_sources: None,
        on_error: None,
    })
    .await
    .expect("create host");
    let detach = connect_canvas(&canvas_svc, in_process(&host), context(), Arc::clone(&logged))
        .await
        .expect("connect canvas");

    let canvas_obj = host.services().use_::<ServiceObject>(&canvas_svc).expect("use Canvas");
    invoke_method(
        &canvas_obj,
        "addStroke",
        vec![
            ServiceValue::from_json(&serde_json::json!({ "color": "red", "points": [] })),
            ServiceValue::Object(ServiceObject::from_context(context())),
        ],
    )
    .await
    .expect("addStroke");

    eventually_sync(|| logged.lock().unwrap_or_else(|e| e.into_inner()).len() == 2).await;
    let summary: Vec<(String, usize)> = logged
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(kind, _, strokes)| (kind.clone(), strokes.as_array().map_or(0, Vec::len)))
        .collect();
    assert_eq!(summary, vec![("hydrate".to_owned(), 1), ("update".to_owned(), 2)]);

    let order = Mutex::new(Vec::new());
    shutdown(&host, || detach(), &harness, &order, context())
        .await
        .expect("shutdown");
    assert_eq!(
        *order.lock().unwrap_or_else(|e| e.into_inner()),
        vec![
            "detach start",
            "detach end",
            "dispose start",
            "dispose end",
            "close start",
            "close end",
        ]
    );

    let err_disposed = host
        .services()
        .use_::<ServiceObject>(&canvas_svc)
        .expect_err("use after dispose must fail");
    assert!(err_disposed.message().contains("disposed"), "got {err_disposed}");

    let err_closed = harness
        .commit(|_tx| async move { Ok(()) }, context())
        .await
        .expect_err("commit after close must fail");
    assert!(err_closed.message().contains("closed"), "got {err_closed}");
}

#[tokio::test]
async fn runs_the_diff_reviews_with_a_keyed_consumer() {
    let doc = review_doc();
    let diff_reviews_svc = svc_diff_reviews();
    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        Vec::new(),
        OpenTasksOptions {
            registry: None,
            now: None,
            settings: None,
        },
    )
    .await
    .expect("open tasks");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let reviews = vec![ReviewSpec {
        key: "review-7".to_owned(),
        seed: ReviewInput {
            path: "a.ts".to_owned(),
            patch: "-old\n+new".to_owned(),
        },
    }];

    let seen: Arc<Mutex<Vec<(String, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let dr_consumer = diff_reviews_svc.clone();
    let seen_cb = Arc::clone(&seen);
    let consumer = define_facet(Facet::new(
        "app.diff-reviews/consumer",
        Arc::new(move |env| {
            let stops: Arc<Mutex<Vec<ServiceObject>>> = Arc::new(Mutex::new(Vec::new()));
            let stops_own = Arc::clone(&stops);
            env.own(
                Arc::new(move || {
                    let list: Vec<ServiceObject> = {
                        let mut guard = stops_own.lock().unwrap_or_else(|e| e.into_inner());
                        std::mem::take(&mut *guard)
                    };
                    for unsub in list {
                        stop_unsubscribe_obj(&unsub)?;
                    }
                    Ok(())
                })
                .into_async_disposal(),
            )?;
            let seen_c = Arc::clone(&seen_cb);
            env.observe::<ServiceObject>(
                &dr_consumer,
                Arc::new(move |review, call_context| {
                    let seen_c = Arc::clone(&seen_c);
                    let stops = Arc::clone(&stops);
                    Box::pin(async move {
                        let id_json = invoke_method(
                            &review,
                            "identity",
                            vec![ServiceValue::Object(ServiceObject::from_context(call_context.clone()))],
                        )
                        .await?
                        .expect("identity response");
                        let key = id_json
                            .get("key")
                            .and_then(|v| v.as_str())
                            .expect("identity key")
                            .to_owned();
                        let stop = subscribe_state_with_delivery(&review, "state", move |val, _del| {
                            let comments = val
                                .as_object()
                                .and_then(|o| o.get("comments"))
                                .and_then(|c| c.as_array())
                                .map(|a| a.len() as i64)
                                .unwrap_or(-1);
                            seen_c
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((key.clone(), comments));
                        })?;
                        stops.lock().unwrap_or_else(|e| e.into_inner()).push(stop);
                        invoke_method(
                            &review,
                            "addComment",
                            vec![
                                ServiceValue::from_json(&serde_json::json!({
                                    "id": "c1",
                                    "line": 1,
                                    "text": "why?",
                                })),
                                ServiceValue::Object(ServiceObject::from_context(call_context)),
                            ],
                        )
                        .await?;
                        Ok(())
                    })
                }),
            )
        }),
    ));

    let provider = review_facet(&harness, doc.clone(), &diff_reviews_svc, root.id(), reviews, context());
    let host = create_facet_host(FacetOptions {
        facets: vec![provider, consumer],
        service_sources: None,
        on_error: None,
    })
    .await
    .expect("create host");

    eventually_sync(|| {
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(_, c)| *c == 1)
    })
    .await;
    assert_eq!(
        *seen.lock().unwrap_or_else(|e| e.into_inner()),
        vec![("review-7".to_owned(), 0), ("review-7".to_owned(), 1)]
    );

    let snap = harness
        .snapshot_conversation_family(&doc, root.id(), "review-7", context())
        .await
        .expect("snapshot family")
        .expect("review present");
    assert_eq!(snap.path, "a.ts");
    assert_eq!(snap.comments.len(), 1);
    assert_eq!(snap.comments[0].id, "c1");

    host.dispose().await.expect("dispose");
    harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn runs_the_job_output_watch_until_the_producer_retires_its_document() {
    let doc = job_output_doc();
    let logged: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sent: Arc<Mutex<Vec<(Option<JobOutput>, Vec<Op>)>>> = Arc::new(Mutex::new(Vec::new()));
    let rendered: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let gate = deferred::<()>();
    let gate_run = gate.clone();
    let doc_run = doc.clone();

    let mut phases: IndexMap<String, TypedPhaseHandler<JobInput, JobState, (), ()>> = IndexMap::new();
    phases.insert(
        "running".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let doc_c = doc_run.clone();
            let gate_c = gate_run.clone();
            Box::pin(async move {
                append_job_output(&doc_c, &runtime, &format!("$ {}\n", task.input.command), ctx.clone()).await?;
                gate_c.wait().await?;
                append_job_output(&doc_c, &runtime, "ok\n", ctx.clone()).await?;
                runtime
                    .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                    .await
            })
        }),
    );

    let job = define_task(TaskDefinition {
        name: "app.job".to_owned(),
        version: 1,
        initial: Arc::new(|_| {
            Ok(JobState {
                phase: "running".to_owned(),
            })
        }),
        phases,
        abort: Arc::new(|_task, _runtime, _ctx| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: PhantomData,
    });

    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![AnyTask::from(job.clone())],
        OpenTasksOptions {
            registry: None,
            now: None,
            settings: None,
        },
    )
    .await
    .expect("open tasks");
    let harness = opened.harness;
    let root = harness.root(context(), None).await.expect("root");
    let root_id = root.id();
    let id = root
        .commit(
            move |tx| async move {
                tx.create_task(
                    &job,
                    &JobInput {
                        command: "make".to_owned(),
                    },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: Some(root_id),
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create job task");

    harness.resume().expect("resume");
    let doc_poll = doc.clone();
    let harness_poll = harness.clone();
    let tid_erased = id.erased();
    support::task_support::eventually(move || {
        let harness_p = harness_poll.clone();
        let doc_p = doc_poll.clone();
        async move {
            harness_p
                .snapshot_task(&doc_p, tid_erased, context())
                .await
                .ok()
                .flatten()
                .is_some_and(|snap| snap.chunks == 1)
        }
    })
    .await;

    let harness_wait = harness.clone();
    let finished = async move {
        let _ = harness_wait.wait_for_task(id, context()).await;
    };
    let observing = observe_job(
        &harness,
        &doc,
        tid_erased,
        finished,
        context(),
        Arc::clone(&logged),
        Arc::clone(&sent),
        Arc::clone(&rendered),
    );
    let logged_poll = Arc::clone(&logged);
    let ((), observe_res) = tokio::join!(
        async move {
            eventually_sync(|| logged_poll.lock().unwrap_or_else(|e| e.into_inner()).len() == 1).await;
            gate.resolve(());
        },
        observing,
    );
    observe_res.expect("observe job");

    assert_eq!(
        *logged.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["$ make\n".to_owned()]
    );
    // Stopped once the task finished; the retirement frame may or may not have been delivered before.
    assert_eq!(
        rendered.lock().unwrap_or_else(|e| e.into_inner()).first().cloned(),
        Some("$ make\nok\n".to_owned())
    );
    assert!(
        harness
            .watch_doc_task(&doc, tid_erased, context())
            .await
            .expect("watch_doc_task")
            .is_none()
    );
    harness.close(context()).await.expect("close");
}

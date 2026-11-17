//! Port of packages/coding-agent/src/experimental/services/connection.ts

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::services::errors::AggregateError;
use pi_chord::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use pi_chord::services::state_internals::get_replicated_state_internals;
use pi_chord::types::{RemoteServiceSourceOpenOptions, ServiceIdRef};
use pi_chord::{
    Context, JsonValue, MutableReplicatedState, RemoteServiceBinding, RemoteServiceBindingOptions, RemoteServiceSource,
    RemoteServiceTransport, RemoteServices, ReplicatedStateDelivery, ServiceCatalogueEntry,
    create_remote_service_binding, replicated_state,
};
use pi_client::{Client, ConnectionState, create_client_service_transport};
use pi_js::{BoxFuture, Unsubscribe};
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{RpcTarget, SessionTarget};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ============================================================================
// Shared Chord ServiceObject & ReplicatedState helpers for experimental services
// ============================================================================

struct MutableStateTarget(MutableReplicatedState<JsonValue>);

impl ServiceTarget for MutableStateTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        match property {
            PropertyKey::String(key) if key == "value" => Ok(ServiceValue::from_json(&self.0.value())),
            PropertyKey::String(key) if key == "subscribe" => {
                let state = self.0.clone();
                Ok(ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                    let Some(ServiceValue::Object(cb)) = args.first().cloned() else {
                        return Err(pi_js::Error::js(
                            "TypeError",
                            "Replicated state subscription listener must be a function",
                        ));
                    };
                    if !cb.is_callable() {
                        return Err(pi_js::Error::js(
                            "TypeError",
                            "Replicated state subscription listener must be a function",
                        ));
                    }
                    let unsub = state.subscribe(Arc::new(move |val, ctx, delivery| {
                        let cb = cb.clone();
                        Box::pin(async move {
                            let delivery_json =
                                serde_json::to_value(delivery).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                            let cb_args = [
                                ServiceValue::from_json(&val),
                                ServiceValue::Object(ServiceObject::from_context(ctx)),
                                ServiceValue::from_json(&delivery_json),
                            ];
                            let outcome = cb.apply(None, &cb_args)?;
                            if let ServiceValue::Object(ret_obj) = outcome
                                && let Some(fut) = ret_obj.into_json_future()
                            {
                                let _ = fut.await?;
                            }
                            Ok(())
                        })
                    }));
                    let cell = Arc::new(Mutex::new(Some(unsub)));
                    Ok(ServiceValue::Object(ServiceObject::from_fn(move |_this, _args| {
                        if let Some(u) = lock(&cell).take() {
                            u.call();
                        }
                        Ok(ServiceValue::Undefined)
                    })))
                })))
            }
            _ => Ok(ServiceValue::Undefined),
        }
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "ReplicatedState is not callable"))
    }
}

/// Wrap a mutable replicated JSON state as a Chord `ServiceObject` state member.
pub fn wrap_mutable_state(state: &MutableReplicatedState<JsonValue>) -> ServiceObject {
    if let Some(internals) = get_replicated_state_internals(&state.as_replicated()) {
        ServiceObject::from_state(MutableStateTarget(state.clone()), internals)
    } else {
        ServiceObject::new(Arc::new(MutableStateTarget(state.clone())))
    }
}

/// Typed wrapper over either a local `MutableReplicatedState<JsonValue>` or a remote `ServiceObject` state proxy.
#[derive(Clone)]
pub enum TypedStateBackend {
    Local(MutableReplicatedState<JsonValue>),
    Remote(ServiceObject),
}

#[derive(Clone)]
pub struct TypedReplicatedState<T> {
    backend: TypedStateBackend,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> TypedReplicatedState<T> {
    pub fn new_local(initial: &T) -> pi_js::Result<Self> {
        let json = serde_json::to_value(initial).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let state = replicated_state(json)?;
        Ok(Self {
            backend: TypedStateBackend::Local(state),
            _marker: PhantomData,
        })
    }

    pub fn from_local_raw(state: MutableReplicatedState<JsonValue>) -> Self {
        Self {
            backend: TypedStateBackend::Local(state),
            _marker: PhantomData,
        }
    }

    pub fn from_remote(state_obj: ServiceObject) -> Self {
        Self {
            backend: TypedStateBackend::Remote(state_obj),
            _marker: PhantomData,
        }
    }

    pub fn from_service_member(service: &ServiceObject, member: &str) -> pi_js::Result<Self> {
        let ServiceValue::Object(state_obj) = service.get_key(member)? else {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("Expected state object member {member}"),
            ));
        };
        Ok(Self::from_remote(state_obj))
    }

    pub fn to_service_object(&self) -> ServiceObject {
        match &self.backend {
            TypedStateBackend::Local(state) => wrap_mutable_state(state),
            TypedStateBackend::Remote(obj) => obj.clone(),
        }
    }

    pub fn value(&self) -> Option<T> {
        self.try_value().ok().flatten()
    }

    pub fn try_value(&self) -> pi_js::Result<Option<T>> {
        let raw = match &self.backend {
            TypedStateBackend::Local(state) => Some(state.value()),
            TypedStateBackend::Remote(obj) => match obj.get_key("value")? {
                ServiceValue::Undefined => None,
                other => Some(other.to_json()?),
            },
        };
        raw.map(|v| serde_json::from_value::<T>(v).map_err(|e| pi_js::Error::msg(e.to_string())))
            .transpose()
    }

    pub fn replace(&self, context: Context, value: &T) -> pi_js::Result<()> {
        match &self.backend {
            TypedStateBackend::Local(state) => {
                let json = serde_json::to_value(value).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                state.replace(context, json)
            }
            TypedStateBackend::Remote(_) => Err(pi_js::Error::msg("Cannot mutate remote replicated state")),
        }
    }

    pub fn change<F>(&self, context: Context, mutate: F) -> pi_js::Result<()>
    where
        F: FnOnce(&mut T) -> pi_js::Result<()>,
    {
        match &self.backend {
            TypedStateBackend::Local(state) => {
                let mut current =
                    serde_json::from_value::<T>(state.value()).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                mutate(&mut current)?;
                let next_json = serde_json::to_value(&current).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                state.replace(context, next_json)
            }
            TypedStateBackend::Remote(_) => Err(pi_js::Error::msg("Cannot mutate remote replicated state")),
        }
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Result<Unsubscribe>
    where
        F: Fn(T, Context, ReplicatedStateDelivery) -> BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
    {
        let listener = Arc::new(listener);
        match &self.backend {
            TypedStateBackend::Local(state) => Ok(state.subscribe(Arc::new(move |json, ctx, delivery| {
                let listener = Arc::clone(&listener);
                Box::pin(async move {
                    let typed = serde_json::from_value::<T>(json).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                    listener(typed, ctx, delivery).await
                })
            }))),
            TypedStateBackend::Remote(obj) => {
                let ServiceValue::Object(sub_fn) = obj.get_key("subscribe")? else {
                    return Err(pi_js::Error::js("TypeError", "Expected state subscribe function"));
                };
                let cb = ServiceObject::from_fn(move |_this, args| {
                    let val_json = args
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
                    let delivery = args
                        .get(2)
                        .map(|v| v.to_json())
                        .transpose()?
                        .and_then(|v| serde_json::from_value::<ReplicatedStateDelivery>(v).ok())
                        .unwrap_or(ReplicatedStateDelivery {
                            kind: pi_chord::types::ReplicatedStateDeliveryKind::Update,
                            sequence: 0,
                        });
                    let listener = Arc::clone(&listener);
                    Ok(ServiceValue::Object(ServiceObject::from_promise(Box::pin(
                        async move {
                            let typed =
                                serde_json::from_value::<T>(val_json).map_err(|e| pi_js::Error::msg(e.to_string()))?;
                            listener(typed, ctx, delivery).await?;
                            Ok(None)
                        },
                    ))))
                });
                let unsub_obj = match sub_fn.apply(None, &[ServiceValue::Object(cb)])? {
                    ServiceValue::Object(o) => o,
                    _ => {
                        return Err(pi_js::Error::js(
                            "TypeError",
                            "subscribe did not return an unsubscribe handle",
                        ));
                    }
                };
                Ok(Unsubscribe::new(move || {
                    let _ = unsub_obj.apply(None, &[]);
                }))
            }
        }
    }
}

pub fn make_async_fn<F>(handler: F) -> ServiceValue
where
    F: Fn(&[ServiceValue], Context) -> BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
        let context = args
            .last()
            .and_then(|v| match v {
                ServiceValue::Object(o) => o.as_context(),
                _ => None,
            })
            .unwrap_or_else(|| BACKGROUND_CONTEXT.clone());
        let business_len = if matches!(args.last(), Some(ServiceValue::Object(o)) if o.as_context().is_some()) {
            args.len().saturating_sub(1)
        } else {
            args.len()
        };
        let fut = handler(&args[..business_len], context);
        Ok(ServiceValue::Object(ServiceObject::from_promise(fut)))
    }))
}

pub fn decode_arg<T: DeserializeOwned>(args: &[ServiceValue], index: usize, name: &str) -> pi_js::Result<T> {
    let val = args
        .get(index)
        .map(|v| v.to_json())
        .transpose()?
        .ok_or_else(|| pi_js::Error::js("TypeError", format!("Missing required argument {name}")))?;
    serde_json::from_value::<T>(val).map_err(|e| pi_js::Error::js("TypeError", format!("Invalid {name}: {e}")))
}

pub async fn invoke_remote_method(
    service: &ServiceObject,
    member: &str,
    args: &[JsonValue],
    context: Context,
) -> pi_js::Result<Option<JsonValue>> {
    let ServiceValue::Object(method) = service.get_key(member)? else {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Expected service method {member}"),
        ));
    };
    let mut sv_args: Vec<ServiceValue> = args.iter().map(ServiceValue::from_json).collect();
    sv_args.push(ServiceValue::Object(ServiceObject::from_context(context)));
    let ServiceValue::Object(promise) = method.apply(None, &sv_args)? else {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Service method {member} did not return a promise"),
        ));
    };
    let Some(future) = promise.into_json_future() else {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Service method {member} promise resolved unexpectedly"),
        ));
    };
    future.await
}

pub async fn invoke_typed<R: DeserializeOwned>(
    service: &ServiceObject,
    member: &str,
    args: &[JsonValue],
    context: Context,
) -> pi_js::Result<R> {
    let result = invoke_remote_method(service, member, args, context)
        .await?
        .unwrap_or(JsonValue::Null);
    serde_json::from_value::<R>(result).map_err(|e| pi_js::Error::msg(e.to_string()))
}

// ============================================================================
// Port of connection.ts
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ServerConnectionState {
    Connecting {
        attempt: u32,
    },
    Connected {
        since: String,
    },
    Disconnected {
        since: String,
        reason: String,
        #[serde(rename = "retryAt")]
        retry_at: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SessionAttachmentState {
    Detached,
    Attaching {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Attached {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Degraded {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

#[derive(Clone, Default)]
pub struct ServiceSourceOptions {
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

struct RoutedServiceBinding {
    services: RemoteServiceBinding,
    get_bound: Arc<dyn Fn() -> bool + Send + Sync>,
    on_activate: Arc<dyn Fn(Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    remove: Arc<dyn Fn() + Send + Sync>,
    activated: Mutex<bool>,
    activation_complete: Mutex<bool>,
}

impl RoutedServiceBinding {
    fn new(
        services: Vec<ServiceIdRef>,
        transport: RemoteServiceTransport,
        get_bound: Arc<dyn Fn() -> bool + Send + Sync>,
        assert_access: Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>,
        on_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
        on_activate: Option<Arc<dyn Fn(Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>>,
        remove: Arc<dyn Fn() + Send + Sync>,
    ) -> pi_js::Result<Arc<Self>> {
        let binding = create_remote_service_binding(RemoteServiceBindingOptions {
            services,
            transport,
            bound: Some(false),
            on_error: Some(on_error),
            assert_access: Some(assert_access),
        })?;
        Ok(Arc::new(Self {
            services: binding,
            get_bound,
            on_activate: on_activate.unwrap_or_else(|| Arc::new(|_| Box::pin(futures::future::ready(Ok(()))))),
            remove,
            activated: Mutex::new(false),
            activation_complete: Mutex::new(false),
        }))
    }

    fn as_remote_services(self: &Arc<Self>) -> RemoteServices {
        let for_use = Arc::clone(self);
        let for_obs = Arc::clone(self);
        let for_ready = Arc::clone(self);
        let for_dispose = Arc::clone(self);
        RemoteServices::new(
            Arc::new(move |service| for_use.services.r#use::<ServiceObject>(service)),
            Arc::new(move |service, handler| for_obs.services.observe::<ServiceObject>(service, handler)),
            Arc::new(move |context| {
                let b = Arc::clone(&for_ready);
                Box::pin(async move { b.ready(context).await })
            }),
            Arc::new(move |context| {
                let b = Arc::clone(&for_dispose);
                Box::pin(async move { b.dispose(context).await })
            }),
        )
    }

    async fn ready(&self, context: Context) -> pi_js::Result<()> {
        let need_activate = {
            let mut activated = lock(&self.activated);
            if !*activated {
                *activated = true;
                true
            } else {
                false
            }
        };
        if need_activate {
            self.services.rebind((self.get_bound)(), context.clone()).await?;
        }
        self.service_ready(context.clone()).await?;
        let need_complete = !*lock(&self.activation_complete);
        if need_complete {
            (self.on_activate)(context).await?;
            *lock(&self.activation_complete) = true;
        }
        Ok(())
    }

    async fn service_ready(&self, context: Context) -> pi_js::Result<()> {
        self.services.ready(context).await
    }

    async fn update_bound(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        if *lock(&self.activated) {
            self.services.rebind(bound, context).await
        } else {
            Ok(())
        }
    }

    async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        (self.remove)();
        self.services.dispose(context).await
    }
}

fn failing_remote_services(err: pi_js::Error) -> RemoteServices {
    let err = Arc::new(err);
    let e1 = Arc::clone(&err);
    let e2 = Arc::clone(&err);
    let e3 = Arc::clone(&err);
    RemoteServices::new(
        Arc::new(move |_| Err(pi_js::Error::js("Error", e1.to_string()))),
        Arc::new(move |_, _| Err(pi_js::Error::js("Error", e2.to_string()))),
        Arc::new(move |_| {
            let msg = e3.to_string();
            Box::pin(async move { Err(pi_js::Error::js("Error", msg)) })
        }),
        Arc::new(move |_| Box::pin(futures::future::ready(Ok(())))),
    )
}

#[derive(Clone)]
pub struct ServerServiceSource {
    pub connection: TypedReplicatedState<ServerConnectionState>,
    inner: Arc<ServerServiceSourceInner>,
    source: RemoteServiceSource,
}

struct ServerServiceSourceInner {
    client: Client,
    transport: RemoteServiceTransport,
    bindings: Mutex<IndexMap<u64, Arc<RoutedServiceBinding>>>,
    next_binding_id: Mutex<u64>,
    transition_lock: tokio::sync::Mutex<()>,
    connection_attempt: Mutex<u32>,
    disposed: Mutex<bool>,
    remove_listener: Mutex<Option<Unsubscribe>>,
}

impl ServerServiceSource {
    pub fn as_remote_source(&self) -> &RemoteServiceSource {
        &self.source
    }

    pub fn into_remote_source(self) -> RemoteServiceSource {
        self.source
    }

    pub async fn catalogue(&self, context: Context) -> pi_js::Result<Vec<ServiceCatalogueEntry>> {
        self.inner
            .client
            .service_catalogue(
                RpcTarget::Server(ServerTarget {
                    server_id: self.inner.client.server_id().to_string(),
                }),
                context.abort_signal(),
            )
            .await
    }

    pub fn open(&self, options: RemoteServiceSourceOpenOptions) -> RemoteServices {
        self.source.open(options)
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        {
            let mut disposed = lock(&self.inner.disposed);
            if *disposed {
                return Ok(());
            }
            *disposed = true;
        }
        if let Some(unsub) = lock(&self.inner.remove_listener).take() {
            unsub.call();
        }
        let _transition = self.inner.transition_lock.lock().await;
        let bindings: Vec<Arc<RoutedServiceBinding>> = {
            let mut guard = lock(&self.inner.bindings);
            guard.drain(..).map(|(_, b)| b).collect()
        };
        let mut errors = Vec::new();
        for binding in bindings {
            if let Err(err) = binding.dispose(context.clone()).await {
                errors.push(err);
            }
        }
        throw_collected_errors(errors, "Failed to dispose server service source")
    }
}

pub fn create_server_service_source(
    client: Client,
    options: ServiceSourceOptions,
) -> pi_js::Result<ServerServiceSource> {
    let client_for_transport = client.clone();
    let transport = create_client_service_transport(&client, move || {
        Some(RpcTarget::Server(ServerTarget {
            server_id: client_for_transport.server_id().to_string(),
        }))
    });
    let on_error = options.on_error.unwrap_or_else(|| Arc::new(|_| {}));
    let initial_attempt = if client.connection_state() == ConnectionState::Connecting {
        1
    } else {
        0
    };
    let connection = TypedReplicatedState::new_local(&to_server_connection_state(&client, initial_attempt, None))?;

    let inner = Arc::new(ServerServiceSourceInner {
        client: client.clone(),
        transport,
        bindings: Mutex::new(IndexMap::new()),
        next_binding_id: Mutex::new(1),
        transition_lock: tokio::sync::Mutex::new(()),
        connection_attempt: Mutex::new(initial_attempt),
        disposed: Mutex::new(false),
        remove_listener: Mutex::new(None),
    });

    let inner_for_listener = Arc::clone(&inner);
    let conn_state = connection.clone();
    let on_error_listener = Arc::clone(&on_error);
    let unsub = client.on_connection_state_change(Arc::new(move |change| {
        let attempt = {
            let mut attempt_guard = lock(&inner_for_listener.connection_attempt);
            if change.state == ConnectionState::Connecting {
                *attempt_guard += 1;
            }
            *attempt_guard
        };
        let next = to_server_connection_state(&inner_for_listener.client, attempt, change.error.as_deref());
        let _ = conn_state.replace(BACKGROUND_CONTEXT.clone(), &next);

        let inner_task = Arc::clone(&inner_for_listener);
        let bound = change.state == ConnectionState::Connected;
        let on_err = Arc::clone(&on_error_listener);
        tokio::spawn(async move {
            let _guard = inner_task.transition_lock.lock().await;
            let bindings: Vec<Arc<RoutedServiceBinding>> = lock(&inner_task.bindings).values().cloned().collect();
            let mut errors = Vec::new();
            for binding in bindings {
                if let Err(err) = binding.update_bound(bound, BACKGROUND_CONTEXT.clone()).await {
                    errors.push(err);
                }
            }
            if let Err(err) = throw_collected_errors(errors, "Failed to rebind server services") {
                on_err(err);
            }
        });
        Ok(())
    }))?;
    *lock(&inner.remove_listener) = Some(unsub);

    let inner_cat = Arc::clone(&inner);
    let inner_open = Arc::clone(&inner);
    let source = RemoteServiceSource::new(
        false,
        Arc::new(move |context| {
            let inner = Arc::clone(&inner_cat);
            Box::pin(async move {
                inner
                    .client
                    .service_catalogue(
                        RpcTarget::Server(ServerTarget {
                            server_id: inner.client.server_id().to_string(),
                        }),
                        context.abort_signal(),
                    )
                    .await
            })
        }),
        Arc::new(move |open_opts| {
            if *lock(&inner_open.disposed) {
                return failing_remote_services(pi_js::Error::msg("Server service source is disposed"));
            }
            let id = {
                let mut next = lock(&inner_open.next_binding_id);
                let id = *next;
                *next += 1;
                id
            };
            let inner_bound = Arc::clone(&inner_open);
            let inner_remove = Arc::clone(&inner_open);
            let binding = match RoutedServiceBinding::new(
                open_opts.services,
                inner_open.transport.clone(),
                Arc::new(move || inner_bound.client.connection_state() == ConnectionState::Connected),
                open_opts.assert_access,
                open_opts.on_error,
                None,
                Arc::new(move || {
                    lock(&inner_remove.bindings).shift_remove(&id);
                }),
            ) {
                Ok(b) => b,
                Err(err) => return failing_remote_services(err),
            };
            lock(&inner_open.bindings).insert(id, Arc::clone(&binding));
            binding.as_remote_services()
        }),
    );

    Ok(ServerServiceSource {
        connection,
        inner,
        source,
    })
}

#[derive(Clone)]
pub struct SessionServiceSource {
    pub attachment: TypedReplicatedState<SessionAttachmentState>,
    inner: Arc<SessionServiceSourceInner>,
    source: RemoteServiceSource,
}

struct SessionServiceSourceInner {
    client: Client,
    transport: RemoteServiceTransport,
    attachment_state: TypedReplicatedState<SessionAttachmentState>,
    on_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
    bindings: Mutex<IndexMap<u64, Arc<RoutedServiceBinding>>>,
    next_binding_id: Mutex<u64>,
    attachment_revision: Mutex<u64>,
    cached_catalogue: Mutex<Option<Vec<ServiceCatalogueEntry>>>,
    disposed: Mutex<bool>,
    transition_lock: tokio::sync::Mutex<()>,
    remove_listener: Mutex<Option<Unsubscribe>>,
}

impl SessionServiceSource {
    pub fn as_remote_source(&self) -> &RemoteServiceSource {
        &self.source
    }

    pub fn into_remote_source(self) -> RemoteServiceSource {
        self.source
    }

    pub async fn catalogue(&self, context: Context) -> pi_js::Result<Vec<ServiceCatalogueEntry>> {
        let Some(target) = self.inner.client.attachment() else {
            return Ok(lock(&self.inner.cached_catalogue).clone().unwrap_or_default());
        };
        let catalogue = self
            .inner
            .client
            .service_catalogue(RpcTarget::Session(target), context.abort_signal())
            .await?;
        *lock(&self.inner.cached_catalogue) = Some(catalogue.clone());
        Ok(catalogue)
    }

    pub fn open(&self, options: RemoteServiceSourceOpenOptions) -> RemoteServices {
        self.source.open(options)
    }

    pub async fn when_attached(&self, session_id: &str, context: Context) -> pi_js::Result<()> {
        let attachment = self.inner.client.attachment();
        let Some(target) = attachment.filter(|t| t.session_id == session_id) else {
            return Err(pi_js::Error::msg(format!(
                "Session {session_id} is not the current attachment"
            )));
        };
        let revision = *lock(&self.inner.attachment_revision);
        when_attached(&self.inner, &target, revision, context).await
    }

    pub async fn when_detached(&self, context: Context) -> pi_js::Result<()> {
        if self.inner.client.attachment().is_some() {
            return Err(pi_js::Error::msg("A Session is still attached"));
        }
        let revision = *lock(&self.inner.attachment_revision);
        when_detached(&self.inner, revision, context).await
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        {
            let mut disposed = lock(&self.inner.disposed);
            if *disposed {
                return Ok(());
            }
            *disposed = true;
        }
        if let Some(unsub) = lock(&self.inner.remove_listener).take() {
            unsub.call();
        }
        let _guard = self.inner.transition_lock.lock().await;
        let bindings: Vec<Arc<RoutedServiceBinding>> = {
            let mut guard = lock(&self.inner.bindings);
            guard.drain(..).map(|(_, b)| b).collect()
        };
        let mut errors = Vec::new();
        for binding in bindings {
            if let Err(err) = binding.dispose(context.clone()).await {
                errors.push(err);
            }
        }
        throw_collected_errors(errors, "Failed to dispose Session service source")
    }
}

pub fn create_session_service_source(
    client: Client,
    options: ServiceSourceOptions,
) -> pi_js::Result<SessionServiceSource> {
    let client_for_transport = client.clone();
    let transport = create_client_service_transport(&client, move || {
        client_for_transport.attachment().map(RpcTarget::Session)
    });
    let on_error = options.on_error.unwrap_or_else(|| Arc::new(|_| {}));
    let initial_state = match client.attachment() {
        None => SessionAttachmentState::Detached,
        Some(target) => SessionAttachmentState::Attaching {
            session_id: target.session_id,
        },
    };
    let attachment = TypedReplicatedState::new_local(&initial_state)?;

    let inner = Arc::new(SessionServiceSourceInner {
        client: client.clone(),
        transport,
        attachment_state: attachment.clone(),
        on_error: Arc::clone(&on_error),
        bindings: Mutex::new(IndexMap::new()),
        next_binding_id: Mutex::new(1),
        attachment_revision: Mutex::new(0),
        cached_catalogue: Mutex::new(None),
        disposed: Mutex::new(false),
        transition_lock: tokio::sync::Mutex::new(()),
        remove_listener: Mutex::new(None),
    });

    let inner_for_listener = Arc::clone(&inner);
    let unsub = client.on_attachment_change(Arc::new(move |next_attachment| {
        let revision = {
            let mut rev = lock(&inner_for_listener.attachment_revision);
            *rev += 1;
            *rev
        };
        if let Some(ref target) = next_attachment {
            let _ = inner_for_listener.attachment_state.replace(
                BACKGROUND_CONTEXT.clone(),
                &SessionAttachmentState::Attaching {
                    session_id: target.session_id.clone(),
                },
            );
            let inner_cat = Arc::clone(&inner_for_listener);
            let target_cat = target.clone();
            tokio::spawn(async move {
                match inner_cat
                    .client
                    .service_catalogue(RpcTarget::Session(target_cat.clone()), None)
                    .await
                {
                    Ok(catalogue) => {
                        if *lock(&inner_cat.attachment_revision) == revision
                            && same_attachment(inner_cat.client.attachment().as_ref(), Some(&target_cat))
                        {
                            *lock(&inner_cat.cached_catalogue) = Some(catalogue);
                        }
                    }
                    Err(err) => {
                        (inner_cat.on_error)(err);
                    }
                }
            });
        }
        let inner_rebind = Arc::clone(&inner_for_listener);
        let bound = next_attachment.is_some();
        tokio::spawn(async move {
            let _guard = inner_rebind.transition_lock.lock().await;
            let bindings: Vec<Arc<RoutedServiceBinding>> = lock(&inner_rebind.bindings).values().cloned().collect();
            let mut errors = Vec::new();
            for binding in bindings {
                if let Err(err) = binding.update_bound(bound, BACKGROUND_CONTEXT.clone()).await {
                    errors.push(err);
                }
            }
            let res = throw_collected_errors(errors, "Failed to rebind Session services");
            if *lock(&inner_rebind.attachment_revision) != revision
                || !same_attachment(inner_rebind.client.attachment().as_ref(), next_attachment.as_ref())
            {
                return;
            }
            match res {
                Ok(()) => {
                    let state = match next_attachment {
                        None => SessionAttachmentState::Detached,
                        Some(ref t) => SessionAttachmentState::Attached {
                            session_id: t.session_id.clone(),
                        },
                    };
                    let _ = inner_rebind
                        .attachment_state
                        .replace(BACKGROUND_CONTEXT.clone(), &state);
                }
                Err(err) => {
                    let state = match next_attachment {
                        None => SessionAttachmentState::Detached,
                        Some(ref t) => SessionAttachmentState::Degraded {
                            session_id: t.session_id.clone(),
                        },
                    };
                    let _ = inner_rebind
                        .attachment_state
                        .replace(BACKGROUND_CONTEXT.clone(), &state);
                    (inner_rebind.on_error)(err);
                }
            }
        });
        Ok(())
    }))?;
    *lock(&inner.remove_listener) = Some(unsub);

    let inner_cat = Arc::clone(&inner);
    let inner_open = Arc::clone(&inner);
    let source = RemoteServiceSource::new(
        true,
        Arc::new(move |context| {
            let inner = Arc::clone(&inner_cat);
            Box::pin(async move {
                let Some(target) = inner.client.attachment() else {
                    return Ok(lock(&inner.cached_catalogue).clone().unwrap_or_default());
                };
                let catalogue = inner
                    .client
                    .service_catalogue(RpcTarget::Session(target), context.abort_signal())
                    .await?;
                *lock(&inner.cached_catalogue) = Some(catalogue.clone());
                Ok(catalogue)
            })
        }),
        Arc::new(move |open_opts| {
            if *lock(&inner_open.disposed) {
                return failing_remote_services(pi_js::Error::msg("Session service source is disposed"));
            }
            let id = {
                let mut next = lock(&inner_open.next_binding_id);
                let id = *next;
                *next += 1;
                id
            };
            let inner_bound = Arc::clone(&inner_open);
            let inner_activate = Arc::clone(&inner_open);
            let inner_remove = Arc::clone(&inner_open);
            let binding = match RoutedServiceBinding::new(
                open_opts.services,
                inner_open.transport.clone(),
                Arc::new(move || inner_bound.client.attachment().is_some()),
                open_opts.assert_access,
                open_opts.on_error,
                Some(Arc::new(move |context| {
                    let inner = Arc::clone(&inner_activate);
                    Box::pin(async move {
                        let rev = *lock(&inner.attachment_revision);
                        match inner.client.attachment() {
                            None => when_detached(&inner, rev, context).await,
                            Some(target) => when_attached(&inner, &target, rev, context).await,
                        }
                    })
                })),
                Arc::new(move || {
                    lock(&inner_remove.bindings).shift_remove(&id);
                }),
            ) {
                Ok(b) => b,
                Err(err) => return failing_remote_services(err),
            };
            lock(&inner_open.bindings).insert(id, Arc::clone(&binding));
            binding.as_remote_services()
        }),
    );

    Ok(SessionServiceSource {
        attachment,
        inner,
        source,
    })
}

async fn when_attached(
    inner: &Arc<SessionServiceSourceInner>,
    attachment: &SessionTarget,
    revision: u64,
    context: Context,
) -> pi_js::Result<()> {
    let bindings: Vec<Arc<RoutedServiceBinding>> = lock(&inner.bindings).values().cloned().collect();
    for binding in bindings {
        if let Err(err) = binding.service_ready(context.clone()).await {
            if *lock(&inner.attachment_revision) == revision
                && same_attachment(inner.client.attachment().as_ref(), Some(attachment))
            {
                let _ = inner.attachment_state.replace(
                    context,
                    &SessionAttachmentState::Degraded {
                        session_id: attachment.session_id.clone(),
                    },
                );
            }
            return Err(err);
        }
    }
    if *lock(&inner.attachment_revision) != revision
        || !same_attachment(inner.client.attachment().as_ref(), Some(attachment))
    {
        return Err(pi_js::Error::msg(format!(
            "Session {} was replaced while attaching",
            attachment.session_id
        )));
    }
    let current = inner.attachment_state.value();
    if !matches!(current, Some(SessionAttachmentState::Attached { ref session_id }) if session_id == &attachment.session_id)
    {
        let _ = inner.attachment_state.replace(
            context,
            &SessionAttachmentState::Attached {
                session_id: attachment.session_id.clone(),
            },
        );
    }
    Ok(())
}

async fn when_detached(inner: &Arc<SessionServiceSourceInner>, revision: u64, context: Context) -> pi_js::Result<()> {
    let bindings: Vec<Arc<RoutedServiceBinding>> = lock(&inner.bindings).values().cloned().collect();
    for binding in bindings {
        binding.service_ready(context.clone()).await?;
    }
    if *lock(&inner.attachment_revision) != revision || inner.client.attachment().is_some() {
        return Err(pi_js::Error::msg("The Session attachment changed while detaching"));
    }
    if !matches!(inner.attachment_state.value(), Some(SessionAttachmentState::Detached)) {
        let _ = inner
            .attachment_state
            .replace(context, &SessionAttachmentState::Detached);
    }
    Ok(())
}

fn to_server_connection_state(client: &Client, attempt: u32, error: Option<&pi_js::Error>) -> ServerConnectionState {
    let since = pi_js::time::iso_now();
    match client.connection_state() {
        ConnectionState::Connecting => ServerConnectionState::Connecting { attempt },
        ConnectionState::Connected => ServerConnectionState::Connected { since },
        ConnectionState::Disconnected => ServerConnectionState::Disconnected {
            since,
            reason: error
                .map(ToString::to_string)
                .unwrap_or_else(|| "Client is disconnected".to_string()),
            retry_at: None,
        },
    }
}

fn same_attachment(left: Option<&SessionTarget>, right: Option<&SessionTarget>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(l), Some(r)) => {
            l.server_id == r.server_id && l.session_id == r.session_id && l.attachment_id == r.attachment_id
        }
        _ => false,
    }
}

fn throw_collected_errors(errors: Vec<pi_js::Error>, message: &str) -> pi_js::Result<()> {
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.into_iter().next().unwrap_or_else(|| pi_js::Error::msg(message))),
        _ => Err(pi_js::Error::typed(
            "AggregateError",
            AggregateError {
                message: message.to_string(),
                errors,
            },
        )),
    }
}

pub fn json_value_from_serde<T: Serialize>(value: &T) -> pi_js::Result<JsonValue> {
    serde_json::to_value(value).map_err(|e| pi_js::Error::msg(e.to_string()))
}

pub fn unit_or_json<T: Serialize>(value: &T) -> pi_js::Result<Option<JsonValue>> {
    Ok(Some(json_value_from_serde(value)?))
}

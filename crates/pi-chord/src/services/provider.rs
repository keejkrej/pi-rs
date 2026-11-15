//! Port of packages/chord/src/services/provider.ts

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use crate::services::errors::{RemoteServiceError, RemoteServiceErrorCode, throw_collected_errors};
use crate::services::handle::{PropertyKey, ServiceObject, ServiceTarget, ServiceValue};
use crate::services::state::service_delivery_context;
use crate::services::state_internals::{ReplicatedStateInternals, get_replicated_state_internals};
use crate::services::wire::{ServiceControlCall, decode_service_control_call};

static NEXT_PROVIDER_SUBSCRIBER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PROVIDER_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

const SUBSCRIBER_BUFFER_LIMIT: usize = 100;

/// `(subscriptionId, update, context) => void | Promise<void>`.
pub type ServiceUpdatePublisher = Arc<
    dyn Fn(String, crate::types::ServiceProviderUpdate, crate::types::Context) -> pi_js::BoxFuture<pi_js::Result<()>>
        + Send
        + Sync,
>;

/// `(...args, context) => Promise<JsonValue | void>`.
pub type RemoteServiceMethod = Arc<
    dyn Fn(
            Vec<crate::types::JsonValue>,
            crate::types::Context,
        ) -> pi_js::BoxFuture<pi_js::Result<Option<crate::types::JsonValue>>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub enum RemoteServiceMember {
    Method(RemoteServiceMethod),
    State(Arc<dyn ReplicatedStateInternals>),
}

// PORT: TypeScript enumerates own data properties (`Object.keys` sorted, data descriptors only).
// Rust has no object-key reflection, so a remote implementation exposes its members here.
pub trait RemoteServiceImplementation: Send + Sync + 'static {
    fn members(&self) -> indexmap::IndexMap<String, RemoteServiceMember>;

    #[doc(hidden)]
    fn as_service_object(&self) -> Option<&ServiceObject> {
        None
    }
}

impl RemoteServiceImplementation for ServiceObject {
    fn members(&self) -> indexmap::IndexMap<String, RemoteServiceMember> {
        classify_service_object("", self).unwrap_or_default()
    }

    fn as_service_object(&self) -> Option<&ServiceObject> {
        Some(self)
    }
}

/// Constructor argument: a bare [`Service`](crate::types::Service) (singleton) or `{ service, mode }`.
pub enum RemoteServiceProviderEntry {
    Service(crate::types::Service),
    Definition {
        service: crate::types::Service,
        mode: crate::types::ServiceMode,
    },
}

/// Hosts one provider for one remote consumer and owns that consumer's subscriptions.
#[derive(Clone)]
pub struct RemoteServiceEndpoint {
    inner: Arc<RemoteServiceEndpointInner>,
}

struct RemoteServiceEndpointInner {
    provider: RemoteServiceProvider,
    subscriptions: Mutex<indexmap::IndexMap<String, RemoteServiceSubscription>>,
    disposed: AtomicBool,
}

impl RemoteServiceEndpoint {
    pub async fn invoke(
        &self,
        call: &crate::types::ServiceCall,
        publish: ServiceUpdatePublisher,
        context: crate::types::Context,
    ) -> pi_js::Result<Option<crate::types::JsonValue>> {
        if self.inner.disposed.load(Ordering::Acquire) {
            return Err(pi_js::Error::js("Error", "Remote service endpoint is disposed"));
        }

        if let Some(control) = decode_service_control_call(call) {
            match control {
                ServiceControlCall::Catalogue => {
                    let value = serde_json::to_value(self.inner.provider.catalogue())
                        .map_err(|err| pi_js::Error::js("Error", err.to_string()))?;
                    return Ok(Some(value));
                }
                ServiceControlCall::Subscribe {
                    subscription_id,
                    service_id,
                    mode,
                } => {
                    {
                        let subscriptions = self.inner.subscriptions.lock().unwrap();
                        if subscriptions.contains_key(&subscription_id) {
                            return Err(pi_js::Error::js("Error", "Service subscription ID is already active"));
                        }
                    }

                    let sub_id_for_listener = subscription_id.clone();
                    let subscription =
                        self.inner
                            .provider
                            .subscribe(&service_id, mode, move |update, update_context| {
                                let mut fut = publish(sub_id_for_listener.clone(), update, update_context);
                                let waker = futures::task::noop_waker_ref();
                                let mut cx = std::task::Context::from_waker(waker);
                                if matches!(fut.as_mut().poll(&mut cx), std::task::Poll::Pending) {
                                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                                        handle.spawn(async move {
                                            let _ = fut.await;
                                        });
                                    } else {
                                        std::thread::spawn(move || {
                                            let _ = futures::executor::block_on(fut);
                                        });
                                    }
                                }
                            })?;

                    {
                        let mut subscriptions = self.inner.subscriptions.lock().unwrap();
                        if self.inner.disposed.load(Ordering::Acquire) {
                            drop(subscriptions);
                            let _ = subscription.close(None);
                            return Err(pi_js::Error::js("Error", "Remote service endpoint is disposed"));
                        }
                        if subscriptions.contains_key(&subscription_id) {
                            drop(subscriptions);
                            let _ = subscription.close(None);
                            return Err(pi_js::Error::js("Error", "Service subscription ID is already active"));
                        }
                        subscriptions.insert(subscription_id.clone(), subscription.clone());
                    }

                    if let Err(error) = subscription.activate() {
                        {
                            let mut subscriptions = self.inner.subscriptions.lock().unwrap();
                            subscriptions.shift_remove(&subscription_id);
                        }
                        let _ = subscription.close(None);
                        return Err(error);
                    }

                    let snapshot = serde_json::to_value(subscription.snapshot())
                        .map_err(|err| pi_js::Error::js("Error", err.to_string()))?;
                    return Ok(Some(snapshot));
                }
                ServiceControlCall::Unsubscribe { subscription_id } => {
                    let subscription = {
                        let mut subscriptions = self.inner.subscriptions.lock().unwrap();
                        subscriptions.shift_remove(&subscription_id)
                    };
                    let Some(subscription) = subscription else {
                        return Err(pi_js::Error::js("Error", "Service subscription was not found"));
                    };
                    subscription.close(None)?;
                    return Ok(None);
                }
            }
        }

        self.inner.provider.invoke(call, context).await
    }

    pub fn dispose(&self) {
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        let subscriptions: Vec<RemoteServiceSubscription> = {
            let mut guard = self.inner.subscriptions.lock().unwrap();
            let drained = std::mem::take(&mut *guard);
            drained.into_values().collect()
        };
        for subscription in subscriptions {
            let _ = subscription.close(None);
        }
    }
}

#[derive(Clone)]
pub struct RemoteServiceProvider {
    inner: Arc<RemoteServiceProviderInner>,
}

struct RemoteServiceProviderInner {
    catalogue: Vec<crate::types::ServiceCatalogueEntry>,
    registrations: Mutex<indexmap::IndexMap<String, ServiceRegistration>>,
    disposed: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceMemberKind {
    Method,
    State,
}

struct ProviderInstance {
    instance_id: u64,
    address: Option<crate::types::ServiceInstanceAddress>,
    implementation: Arc<dyn std::any::Any + Send + Sync>,
    members: indexmap::IndexMap<String, RemoteServiceMember>,
    remove_member_listeners: Vec<pi_js::Unsubscribe>,
    active: Arc<AtomicBool>,
}

type ServiceProviderListener =
    Arc<dyn Fn(crate::types::ServiceProviderUpdate, crate::types::Context) -> pi_js::Result<()> + Send + Sync>;

struct ProviderDelivery {
    update: crate::types::ServiceProviderUpdate,
    context: crate::types::Context,
}

struct ProviderSubscriberState {
    buffer: Vec<ProviderDelivery>,
    snapshot_sequences: indexmap::IndexMap<String, i64>,
    active: bool,
    draining: bool,
    terminated: bool,
    closed: bool,
}

#[derive(Clone)]
struct ProviderSubscriber {
    inner: Arc<ProviderSubscriberInner>,
}

struct ProviderSubscriberInner {
    id: u64,
    listener: ServiceProviderListener,
    state: Mutex<ProviderSubscriberState>,
}

struct ServiceRegistration {
    service_id: String,
    mode: crate::types::ServiceMode,
    singleton: Option<ProviderInstance>,
    singleton_shape: Option<indexmap::IndexMap<String, ServiceMemberKind>>,
    instances: indexmap::IndexMap<String, ProviderInstance>,
    generations: indexmap::IndexMap<String, i64>,
    subscribers: indexmap::IndexMap<u64, ProviderSubscriber>,
}

/// Object returned by [`RemoteServiceProvider::subscribe`]. Implements the `ServiceSubscription` contract.
#[derive(Clone)]
pub struct RemoteServiceSubscription {
    inner: Arc<RemoteServiceSubscriptionInner>,
}

struct RemoteServiceSubscriptionInner {
    snapshot: crate::types::ServiceSubscriptionSnapshot,
    subscriber: ProviderSubscriber,
    service_id: String,
    provider: RemoteServiceProvider,
}

impl RemoteServiceSubscription {
    pub fn snapshot(&self) -> &crate::types::ServiceSubscriptionSnapshot {
        &self.inner.snapshot
    }

    pub fn activate(&self) -> pi_js::Result<()> {
        {
            let mut state = self.inner.subscriber.inner.state.lock().unwrap();
            if state.closed || state.active {
                return Ok(());
            }
            state.active = true;
        }
        let errors = drain_subscriber(&self.inner.subscriber);
        throw_collected_errors(&errors, "Failed to activate remote service subscription")
    }

    pub fn close(&self, _context: Option<crate::types::Context>) -> pi_js::Result<()> {
        {
            let mut registrations = self.inner.provider.inner.registrations.lock().unwrap();
            if let Some(registration) = registrations.get_mut(&self.inner.service_id) {
                registration.subscribers.shift_remove(&self.inner.subscriber.inner.id);
            }
        }
        let mut state = self.inner.subscriber.inner.state.lock().unwrap();
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        state.buffer.clear();
        Ok(())
    }
}

struct ClassifiedRemoteServiceImplementation {
    object: Arc<dyn std::any::Any + Send + Sync>,
    members: indexmap::IndexMap<String, RemoteServiceMember>,
}

impl RemoteServiceProvider {
    pub fn new(entries: Vec<RemoteServiceProviderEntry>) -> pi_js::Result<Self> {
        let mut definitions = Vec::with_capacity(entries.len());
        let mut seen_ids = indexmap::IndexSet::new();

        for entry in entries {
            let (service, mode) = match entry {
                RemoteServiceProviderEntry::Service(service) => (service, crate::types::ServiceMode::Singleton),
                RemoteServiceProviderEntry::Definition { service, mode } => (service, mode),
            };
            if service.local {
                return Err(pi_js::Error::js(
                    "TypeError",
                    format!("Local service {} cannot be published remotely", service.id),
                ));
            }
            if !seen_ids.insert(service.id.clone()) {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Remote service catalogue contains duplicate IDs",
                ));
            }
            definitions.push((service, mode));
        }

        let mut catalogue = Vec::with_capacity(definitions.len());
        let mut registrations = indexmap::IndexMap::with_capacity(definitions.len());
        for (service, mode) in definitions {
            catalogue.push(crate::types::ServiceCatalogueEntry {
                service_id: service.id.clone(),
                mode,
            });
            registrations.insert(
                service.id.clone(),
                ServiceRegistration {
                    service_id: service.id,
                    mode,
                    singleton: None,
                    singleton_shape: None,
                    instances: indexmap::IndexMap::new(),
                    generations: indexmap::IndexMap::new(),
                    subscribers: indexmap::IndexMap::new(),
                },
            );
        }

        Ok(Self {
            inner: Arc::new(RemoteServiceProviderInner {
                catalogue,
                registrations: Mutex::new(registrations),
                disposed: AtomicBool::new(false),
            }),
        })
    }

    pub fn catalogue(&self) -> &[crate::types::ServiceCatalogueEntry] {
        &self.inner.catalogue
    }

    pub fn provide<T>(&self, service: &crate::types::Service, implementation: Arc<T>) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        self.assert_active()?;
        self.assert_remotable(service)?;
        {
            let registrations = self.inner.registrations.lock().unwrap();
            let registration =
                self.registration_ref(&registrations, &service.id, crate::types::ServiceMode::Singleton)?;
            if registration.singleton.is_some() {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!("Remote service {} already has a provider", service.id),
                )
                .into_js_error());
            }
        }

        let classified =
            classify_remote_service_implementation(&service.id, implementation.as_ref(), implementation.clone())?;
        let shape = service_member_shape(&classified.members);

        let mut registrations = self.inner.registrations.lock().unwrap();
        self.assert_active()?;
        let registration =
            self.registration_mut(&mut registrations, &service.id, crate::types::ServiceMode::Singleton)?;
        if registration.singleton.is_some() {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!("Remote service {} already has a provider", service.id),
            )
            .into_js_error());
        }
        self.assert_singleton_shape(&registration.service_id, registration.singleton_shape.as_ref(), &shape)?;
        let instance = self.create_instance(&registration.service_id, classified, None)?;
        registration.singleton = Some(instance);
        registration.singleton_shape = Some(shape);
        Ok(())
    }

    /// Disconnect one singleton while preserving active subscriptions and remote facades.
    pub fn withdraw(&self, service: &crate::types::Service) -> pi_js::Result<()> {
        self.assert_active()?;
        self.assert_remotable(service)?;

        let (unsubscribes, subscribers) = {
            let mut registrations = self.inner.registrations.lock().unwrap();
            let registration =
                self.registration_mut(&mut registrations, &service.id, crate::types::ServiceMode::Singleton)?;
            let Some(mut previous) = registration.singleton.take() else {
                return Ok(());
            };
            previous.active.store(false, Ordering::Release);
            let unsubscribes = std::mem::take(&mut previous.remove_member_listeners);
            let subscribers = queue_emit_locked(registration, crate::types::ServiceProviderUpdate::Unavailable, None);
            (unsubscribes, subscribers)
        };

        for remove in unsubscribes {
            remove.call();
        }
        drain_subscribers_and_throw(&subscribers, &service.id)
    }

    /// Check a singleton replacement without changing the active provider.
    pub fn validate_replacement<T>(&self, service: &crate::types::Service, implementation: &T) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        self.assert_active()?;
        self.assert_remotable(service)?;
        let current_shape = {
            let registrations = self.inner.registrations.lock().unwrap();
            let registration =
                self.registration_ref(&registrations, &service.id, crate::types::ServiceMode::Singleton)?;
            registration.singleton_shape.clone()
        };
        let members = classify_implementation_members(&service.id, implementation, None)?;
        let replacement_shape = service_member_shape(&members);
        self.assert_singleton_shape(&service.id, current_shape.as_ref(), &replacement_shape)
    }

    /// Replace one singleton without making its stable remote facade unavailable.
    pub fn replace<T>(&self, service: &crate::types::Service, implementation: Arc<T>) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        self.assert_active()?;
        self.assert_remotable(service)?;
        {
            let registrations = self.inner.registrations.lock().unwrap();
            let _ = self.registration_ref(&registrations, &service.id, crate::types::ServiceMode::Singleton)?;
        }

        let classified =
            classify_remote_service_implementation(&service.id, implementation.as_ref(), implementation.clone())?;
        let shape = service_member_shape(&classified.members);

        let (unsubscribes, subscribers) = {
            let mut registrations = self.inner.registrations.lock().unwrap();
            self.assert_active()?;
            let registration =
                self.registration_mut(&mut registrations, &service.id, crate::types::ServiceMode::Singleton)?;
            self.assert_singleton_shape(&registration.service_id, registration.singleton_shape.as_ref(), &shape)?;
            let replacement = self.create_instance(&registration.service_id, classified, None)?;
            let unsubscribes = if let Some(mut previous) = registration.singleton.take() {
                previous.active.store(false, Ordering::Release);
                std::mem::take(&mut previous.remove_member_listeners)
            } else {
                Vec::new()
            };
            let snapshot = self.snapshot_instance(&replacement);
            registration.singleton = Some(replacement);
            registration.singleton_shape = Some(shape);
            let subscribers = queue_emit_locked(
                registration,
                crate::types::ServiceProviderUpdate::Replaced { snapshot },
                None,
            );
            (unsubscribes, subscribers)
        };

        for remove in unsubscribes {
            remove.call();
        }
        drain_subscribers_and_throw(&subscribers, &service.id)
    }

    pub fn use_<T>(&self, service: &crate::types::Service) -> pi_js::Result<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.assert_active()?;
        self.assert_remotable(service)?;
        let registrations = self.inner.registrations.lock().unwrap();
        self.assert_allowed_locked(&registrations, &service.id)?;
        let registration = registrations.get(&service.id);
        let Some(singleton) = registration
            .filter(|reg| reg.mode == crate::types::ServiceMode::Singleton)
            .and_then(|reg| reg.singleton.as_ref())
        else {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Remote service {} has no local provider", service.id),
            )
            .into_js_error());
        };

        if let Ok(exact) = Arc::clone(&singleton.implementation).downcast::<T>() {
            return Ok(exact);
        }
        if TypeId::of::<T>() == TypeId::of::<ServiceObject>() {
            let object = service_object_from_members(&singleton.members);
            let erased: Arc<dyn Any + Send + Sync> = Arc::new(object);
            return Ok(erased.downcast::<T>().expect("ServiceObject TypeId verified"));
        }
        Err(pi_js::Error::js(
            "TypeError",
            format!(
                "Remote service {} implementation does not match requested type",
                service.id
            ),
        ))
    }

    pub fn spawn<T>(
        &self,
        service: &crate::types::Service,
        key: &str,
        implementation: Arc<T>,
    ) -> pi_js::Result<pi_js::Unsubscribe>
    where
        T: RemoteServiceImplementation,
    {
        let close = self.spawn_with_fallible_close(service, key, implementation)?;
        Ok(pi_js::Unsubscribe::new(move || {
            let _ = close();
        }))
    }

    pub(crate) fn spawn_with_fallible_close<T>(
        &self,
        service: &crate::types::Service,
        key: &str,
        implementation: Arc<T>,
    ) -> pi_js::Result<Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>>
    where
        T: RemoteServiceImplementation,
    {
        self.assert_active()?;
        self.assert_remotable(service)?;
        {
            let registrations = self.inner.registrations.lock().unwrap();
            self.assert_allowed_locked(&registrations, &service.id)?;
            if key.is_empty() {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Remote service instance key must not be empty",
                ));
            }
            let registration = self.registration_ref(&registrations, &service.id, crate::types::ServiceMode::Keyed)?;
            if registration.instances.contains_key(key) {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!(
                        "Remote service {} already has a live instance with key {key}",
                        service.id
                    ),
                )
                .into_js_error());
            }
        }
        if key.is_empty() {
            return Err(pi_js::Error::js(
                "TypeError",
                "Remote service instance key must not be empty",
            ));
        }

        let classified =
            classify_remote_service_implementation(&service.id, implementation.as_ref(), implementation.clone())?;

        let (instance_id, address, subscribers) = {
            let mut registrations = self.inner.registrations.lock().unwrap();
            self.assert_active()?;
            let registration =
                self.registration_mut(&mut registrations, &service.id, crate::types::ServiceMode::Keyed)?;
            if registration.instances.contains_key(key) {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!(
                        "Remote service {} already has a live instance with key {key}",
                        service.id
                    ),
                )
                .into_js_error());
            }
            let generation = registration.generations.get(key).copied().unwrap_or(0) + 1;
            registration.generations.insert(key.to_string(), generation);
            let address = crate::types::ServiceInstanceAddress {
                key: key.to_string(),
                generation,
            };
            let instance = self.create_instance(&registration.service_id, classified, Some(address.clone()))?;
            let instance_id = instance.instance_id;
            let snapshot = self.snapshot_instance(&instance);
            registration.instances.insert(key.to_string(), instance);
            let subscribers = queue_emit_locked(
                registration,
                crate::types::ServiceProviderUpdate::Spawned { instance: snapshot },
                None,
            );
            (instance_id, address, subscribers)
        };

        drain_subscribers_and_throw(&subscribers, &service.id)?;

        let provider = self.clone();
        let service_id = service.id.clone();
        let key_owned = key.to_string();
        let closed = AtomicBool::new(false);
        Ok(Arc::new(move || {
            if closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let (unsubscribes, subscribers) = {
                let mut registrations = provider.inner.registrations.lock().unwrap();
                let Some(registration) = registrations.get_mut(&service_id) else {
                    return Ok(());
                };
                if registration
                    .instances
                    .get(&key_owned)
                    .is_none_or(|inst| inst.instance_id != instance_id)
                {
                    return Ok(());
                }
                let Some(mut instance) = registration.instances.shift_remove(&key_owned) else {
                    return Ok(());
                };
                instance.active.store(false, Ordering::Release);
                let unsubscribes = std::mem::take(&mut instance.remove_member_listeners);
                let subscribers = queue_emit_locked(
                    registration,
                    crate::types::ServiceProviderUpdate::Closed {
                        instance: address.clone(),
                    },
                    None,
                );
                (unsubscribes, subscribers)
            };
            for remove in unsubscribes {
                remove.call();
            }
            drain_subscribers_and_throw(&subscribers, &service_id)
        }))
    }

    pub async fn invoke(
        &self,
        call: &crate::types::ServiceCall,
        context: crate::types::Context,
    ) -> pi_js::Result<Option<crate::types::JsonValue>> {
        let method = {
            self.assert_active()?;
            let registrations = self.inner.registrations.lock().unwrap();
            self.assert_allowed_locked(&registrations, &call.service_id)?;
            let registration = registrations.get(&call.service_id).ok_or_else(|| {
                RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceNotFound,
                    format!("Unknown remote service {}", call.service_id),
                )
                .into_js_error()
            })?;
            let instance = self.resolve_instance_ref(registration, call.instance.as_ref())?;
            let member = instance.members.get(&call.member).ok_or_else(|| {
                RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceMemberNotFound,
                    format!("Unknown remote service member {}.{}", call.service_id, call.member),
                )
                .into_js_error()
            })?;
            match member {
                RemoteServiceMember::Method(method) => Arc::clone(method),
                RemoteServiceMember::State(_) => {
                    return Err(RemoteServiceError::new(
                        RemoteServiceErrorCode::ServiceMemberMismatch,
                        format!(
                            "Remote service member {}.{} is not a method",
                            call.service_id, call.member
                        ),
                    )
                    .into_js_error());
                }
            }
        };

        method(call.args.clone(), context).await
    }

    pub fn subscribe<F>(
        &self,
        service_id: &str,
        mode: crate::types::ServiceMode,
        listener: F,
    ) -> pi_js::Result<RemoteServiceSubscription>
    where
        F: Fn(crate::types::ServiceProviderUpdate, crate::types::Context) + Send + Sync + 'static,
    {
        self.subscribe_fallible(service_id, mode, move |update, context| {
            listener(update, context);
            Ok(())
        })
    }

    pub fn subscribe_fallible<F>(
        &self,
        service_id: &str,
        mode: crate::types::ServiceMode,
        listener: F,
    ) -> pi_js::Result<RemoteServiceSubscription>
    where
        F: Fn(crate::types::ServiceProviderUpdate, crate::types::Context) -> pi_js::Result<()> + Send + Sync + 'static,
    {
        self.assert_active()?;
        let mut registrations = self.inner.registrations.lock().unwrap();
        let registration = self.registration_mut(&mut registrations, service_id, mode)?;
        if registration.mode == crate::types::ServiceMode::Singleton && registration.singleton.is_none() {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Remote service {service_id} has no provider"),
            )
            .into_js_error());
        }

        let subscriber_id = NEXT_PROVIDER_SUBSCRIBER_ID.fetch_add(1, Ordering::Relaxed);
        let mut snapshot_sequences = indexmap::IndexMap::new();
        let snapshot = snapshot_registration(registration);
        record_snapshot_sequences(&mut snapshot_sequences, &snapshot.instances);

        let subscriber = ProviderSubscriber {
            inner: Arc::new(ProviderSubscriberInner {
                id: subscriber_id,
                listener: Arc::new(listener),
                state: Mutex::new(ProviderSubscriberState {
                    buffer: Vec::new(),
                    snapshot_sequences,
                    active: false,
                    draining: false,
                    terminated: false,
                    closed: false,
                }),
            }),
        };
        registration.subscribers.insert(subscriber_id, subscriber.clone());

        Ok(RemoteServiceSubscription {
            inner: Arc::new(RemoteServiceSubscriptionInner {
                snapshot,
                subscriber,
                service_id: service_id.to_string(),
                provider: self.clone(),
            }),
        })
    }

    pub fn dispose(&self) -> pi_js::Result<()> {
        if self.inner.disposed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        let mut unsubscribes: Vec<pi_js::Unsubscribe> = Vec::new();
        let mut drain_batches: Vec<(String, Vec<ProviderSubscriber>)> = Vec::new();

        {
            let mut registrations = self.inner.registrations.lock().unwrap();
            let drained = std::mem::take(&mut *registrations);
            for (_service_id, mut registration) in drained {
                if let Some(mut singleton) = registration.singleton.take() {
                    singleton.active.store(false, Ordering::Release);
                    unsubscribes.extend(std::mem::take(&mut singleton.remove_member_listeners));
                    let subs = queue_emit_locked(
                        &mut registration,
                        crate::types::ServiceProviderUpdate::Unavailable,
                        None,
                    );
                    drain_batches.push((registration.service_id.clone(), subs));
                }

                let existing_instances = std::mem::take(&mut registration.instances);
                for (_key, mut instance) in existing_instances {
                    instance.active.store(false, Ordering::Release);
                    unsubscribes.extend(std::mem::take(&mut instance.remove_member_listeners));
                    if let Some(address) = instance.address {
                        let subs = queue_emit_locked(
                            &mut registration,
                            crate::types::ServiceProviderUpdate::Closed { instance: address },
                            None,
                        );
                        drain_batches.push((registration.service_id.clone(), subs));
                    }
                }

                // Snapshot subscribers to finalize after queued disposal updates finish draining.
                drain_batches.push((
                    registration.service_id.clone(),
                    registration.subscribers.values().cloned().collect(),
                ));
                registration.subscribers.clear();
            }
        }

        for remove in unsubscribes {
            remove.call();
        }

        let mut errors: Vec<pi_js::Error> = Vec::new();
        for (_service_id, subscribers) in drain_batches {
            for subscriber in subscribers {
                errors.extend(drain_subscriber(&subscriber));
                let mut state = subscriber.inner.state.lock().unwrap();
                if state.active && !state.draining {
                    state.closed = true;
                    state.buffer.clear();
                } else {
                    state.terminated = true;
                }
            }
        }

        throw_collected_errors(&errors, "Failed to dispose remote service provider")
    }

    fn registration_ref<'a>(
        &self,
        registrations: &'a indexmap::IndexMap<String, ServiceRegistration>,
        service_id: &str,
        mode: crate::types::ServiceMode,
    ) -> pi_js::Result<&'a ServiceRegistration> {
        self.assert_allowed_locked(registrations, service_id)?;
        let registration = registrations.get(service_id).ok_or_else(|| {
            RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Unknown remote service {service_id}"),
            )
            .into_js_error()
        })?;
        if registration.mode != mode {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!(
                    "Remote service {service_id} is {}, not {}",
                    mode_str(registration.mode),
                    mode_str(mode)
                ),
            )
            .into_js_error());
        }
        Ok(registration)
    }

    fn registration_mut<'a>(
        &self,
        registrations: &'a mut indexmap::IndexMap<String, ServiceRegistration>,
        service_id: &str,
        mode: crate::types::ServiceMode,
    ) -> pi_js::Result<&'a mut ServiceRegistration> {
        self.assert_allowed_locked(registrations, service_id)?;
        let registration = registrations.get_mut(service_id).ok_or_else(|| {
            RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotFound,
                format!("Unknown remote service {service_id}"),
            )
            .into_js_error()
        })?;
        if registration.mode != mode {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!(
                    "Remote service {service_id} is {}, not {}",
                    mode_str(registration.mode),
                    mode_str(mode)
                ),
            )
            .into_js_error());
        }
        Ok(registration)
    }

    fn create_instance(
        &self,
        service_id: &str,
        classified: ClassifiedRemoteServiceImplementation,
        address: Option<crate::types::ServiceInstanceAddress>,
    ) -> pi_js::Result<ProviderInstance> {
        let active = Arc::new(AtomicBool::new(true));
        let mut remove_member_listeners = Vec::new();
        for (name, member) in &classified.members {
            let RemoteServiceMember::State(state) = member else {
                continue;
            };
            let provider = self.clone();
            let service_id_owned = service_id.to_string();
            let member_name = name.clone();
            let addr = address.clone();
            let instance_active = Arc::clone(&active);
            let unsubscribe = state.subscribe(Arc::new(move |ops, sequence, context| {
                if !instance_active.load(Ordering::Acquire) {
                    return;
                }
                let _ = provider.emit(
                    &service_id_owned,
                    crate::types::ServiceProviderUpdate::State {
                        instance: addr.clone(),
                        member: member_name.clone(),
                        sequence,
                        ops: ops.to_vec(),
                    },
                    Some(context),
                );
            }));
            remove_member_listeners.push(unsubscribe);
        }

        Ok(ProviderInstance {
            instance_id: NEXT_PROVIDER_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
            address,
            implementation: classified.object,
            members: classified.members,
            remove_member_listeners,
            active,
        })
    }

    fn assert_singleton_shape(
        &self,
        service_id: &str,
        current: Option<&indexmap::IndexMap<String, ServiceMemberKind>>,
        replacement: &indexmap::IndexMap<String, ServiceMemberKind>,
    ) -> pi_js::Result<()> {
        let Some(current) = current else {
            return Ok(());
        };
        if same_service_member_shape(current, replacement) {
            return Ok(());
        }
        Err(RemoteServiceError::new(
            RemoteServiceErrorCode::ServiceMemberMismatch,
            format!("Remote service {service_id} replacement must preserve its member shape"),
        )
        .into_js_error())
    }

    fn resolve_instance_ref<'a>(
        &self,
        registration: &'a ServiceRegistration,
        address: Option<&crate::types::ServiceInstanceAddress>,
    ) -> pi_js::Result<&'a ProviderInstance> {
        if registration.mode == crate::types::ServiceMode::Singleton {
            if address.is_some() {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!("Remote service {} is singleton", registration.service_id),
                )
                .into_js_error());
            }
            return registration.singleton.as_ref().ok_or_else(|| {
                RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceNotFound,
                    format!("Remote service {} has no provider", registration.service_id),
                )
                .into_js_error()
            });
        }
        let Some(address) = address else {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceModeMismatch,
                format!("Remote service {} is keyed", registration.service_id),
            )
            .into_js_error());
        };
        let Some(instance) = registration.instances.get(&address.key) else {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceInstanceNotFound,
                format!(
                    "Remote service {} has no instance {}",
                    registration.service_id, address.key
                ),
            )
            .into_js_error());
        };
        let current_generation = instance.address.as_ref().map(|a| a.generation);
        if current_generation != Some(address.generation) {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceStaleInstance,
                format!(
                    "Remote service {} instance {} is stale",
                    registration.service_id, address.key
                ),
            )
            .into_js_error());
        }
        Ok(instance)
    }

    fn snapshot_instance(&self, instance: &ProviderInstance) -> crate::types::ServiceInstanceSnapshot {
        snapshot_instance(instance)
    }

    // Queue for everyone before invoking user code, including reentrant publications.
    fn emit(
        &self,
        service_id: &str,
        update: crate::types::ServiceProviderUpdate,
        context: Option<crate::types::Context>,
    ) -> pi_js::Result<()> {
        let subscribers = {
            let mut registrations = self.inner.registrations.lock().unwrap();
            let Some(registration) = registrations.get_mut(service_id) else {
                return Ok(());
            };
            queue_emit_locked(registration, update, context)
        };
        drain_subscribers_and_throw(&subscribers, service_id)
    }

    fn assert_remotable(&self, service: &crate::types::Service) -> pi_js::Result<()> {
        if service.local {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Service {} is process-local", service.id),
            )
            .into_js_error());
        }
        Ok(())
    }

    fn assert_allowed_locked(
        &self,
        registrations: &indexmap::IndexMap<String, ServiceRegistration>,
        service_id: &str,
    ) -> pi_js::Result<()> {
        if !registrations.contains_key(service_id) {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Remote service {service_id} is not allowlisted"),
            )
            .into_js_error());
        }
        Ok(())
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        if self.inner.disposed.load(Ordering::Acquire) {
            return Err(pi_js::Error::js("Error", "Remote service provider is disposed"));
        }
        Ok(())
    }
}

pub fn create_remote_service_endpoint(provider: &RemoteServiceProvider) -> RemoteServiceEndpoint {
    RemoteServiceEndpoint {
        inner: Arc::new(RemoteServiceEndpointInner {
            provider: provider.clone(),
            subscriptions: Mutex::new(indexmap::IndexMap::new()),
            disposed: AtomicBool::new(false),
        }),
    }
}

pub fn validate_remote_service_implementation(
    service_id: &str,
    implementation: &dyn RemoteServiceImplementation,
) -> pi_js::Result<()> {
    let _ = classify_implementation_members(service_id, implementation, None)?;
    Ok(())
}

fn queue_emit_locked(
    registration: &mut ServiceRegistration,
    update: crate::types::ServiceProviderUpdate,
    context: Option<crate::types::Context>,
) -> Vec<ProviderSubscriber> {
    if registration.subscribers.is_empty() {
        return Vec::new();
    }
    let delivery_context = context.unwrap_or_else(service_delivery_context);
    let subscribers: Vec<ProviderSubscriber> = registration.subscribers.values().cloned().collect();
    for subscriber in &subscribers {
        let mut state = subscriber.inner.state.lock().unwrap();
        if state.closed || update_covered_by_snapshot(&mut state.snapshot_sequences, &update) {
            continue;
        }
        if state.buffer.len() >= SUBSCRIBER_BUFFER_LIMIT {
            let snapshot = snapshot_registration(registration);
            state.buffer.clear();
            state.snapshot_sequences.clear();
            record_snapshot_sequences(&mut state.snapshot_sequences, &snapshot.instances);
            state.buffer.push(ProviderDelivery {
                update: crate::types::ServiceProviderUpdate::Reset { snapshot },
                context: delivery_context.clone(),
            });
        } else {
            state.buffer.push(ProviderDelivery {
                update: update.clone(),
                context: delivery_context.clone(),
            });
        }
    }
    subscribers
}

fn drain_subscribers_and_throw(subscribers: &[ProviderSubscriber], service_id: &str) -> pi_js::Result<()> {
    let mut errors = Vec::new();
    for subscriber in subscribers {
        errors.extend(drain_subscriber(subscriber));
    }
    throw_collected_errors(
        &errors,
        &format!("Failed to publish remote service {service_id} update"),
    )
}

struct SubscriberDrainingGuard<'a>(&'a Mutex<ProviderSubscriberState>);

impl Drop for SubscriberDrainingGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.draining = false;
        if state.terminated {
            state.closed = true;
        }
    }
}

fn drain_subscriber(subscriber: &ProviderSubscriber) -> Vec<pi_js::Error> {
    {
        let mut state = subscriber.inner.state.lock().unwrap();
        if !state.active || state.closed || state.draining {
            return Vec::new();
        }
        state.draining = true;
    }

    let mut errors = Vec::new();
    let _guard = SubscriberDrainingGuard(&subscriber.inner.state);
    loop {
        let entry = {
            let mut state = subscriber.inner.state.lock().unwrap();
            if state.closed || state.buffer.is_empty() {
                break;
            }
            state.buffer.remove(0)
        };
        if let Err(error) = (subscriber.inner.listener)(entry.update, entry.context) {
            errors.push(error);
        }
    }

    errors
}

fn snapshot_registration(registration: &ServiceRegistration) -> crate::types::ServiceSubscriptionSnapshot {
    let instances = match registration.mode {
        crate::types::ServiceMode::Singleton => registration
            .singleton
            .as_ref()
            .map(snapshot_instance)
            .into_iter()
            .collect(),
        crate::types::ServiceMode::Keyed => {
            let mut list: Vec<&ProviderInstance> = registration.instances.values().collect();
            list.sort_by(|left, right| {
                let left_key = left.address.as_ref().map(|a| a.key.as_str()).unwrap_or("");
                let right_key = right.address.as_ref().map(|a| a.key.as_str()).unwrap_or("");
                left_key.cmp(right_key)
            });
            list.into_iter().map(snapshot_instance).collect()
        }
    };
    crate::types::ServiceSubscriptionSnapshot {
        service_id: registration.service_id.clone(),
        mode: registration.mode,
        instances,
    }
}

fn snapshot_instance(instance: &ProviderInstance) -> crate::types::ServiceInstanceSnapshot {
    let mut members = Vec::with_capacity(instance.members.len());
    for (name, member) in &instance.members {
        match member {
            RemoteServiceMember::Method(_) => {
                members.push(crate::types::ServiceMemberSnapshot::Method(
                    crate::types::ServiceMethodSnapshot {
                        name: name.clone(),
                        kind: crate::types::ServiceMemberMethodKind::Method,
                    },
                ));
            }
            RemoteServiceMember::State(state) => {
                let snapshot = state.snapshot();
                members.push(crate::types::ServiceMemberSnapshot::State(
                    crate::types::ServiceStateSnapshot {
                        name: name.clone(),
                        kind: crate::types::ServiceMemberStateKind::State,
                        sequence: snapshot.sequence,
                        ops: vec![crate::delta::Op::Replace(crate::delta::OpReplace(
                            crate::delta::ReplaceVerb::R,
                            snapshot.value,
                        ))],
                    },
                ));
            }
        }
    }
    crate::types::ServiceInstanceSnapshot {
        instance: instance.address.clone(),
        members,
    }
}

fn record_snapshot_sequences(
    sequences: &mut indexmap::IndexMap<String, i64>,
    instances: &[crate::types::ServiceInstanceSnapshot],
) {
    for instance in instances {
        for member in &instance.members {
            if let crate::types::ServiceMemberSnapshot::State(state_snap) = member {
                sequences.insert(
                    state_member_key(instance.instance.as_ref(), &state_snap.name),
                    state_snap.sequence,
                );
            }
        }
    }
}

fn update_covered_by_snapshot(
    sequences: &mut indexmap::IndexMap<String, i64>,
    update: &crate::types::ServiceProviderUpdate,
) -> bool {
    match update {
        crate::types::ServiceProviderUpdate::State {
            instance,
            member,
            sequence,
            ..
        } => {
            let key = state_member_key(instance.as_ref(), member);
            let Some(&seq) = sequences.get(&key) else {
                return false;
            };
            if *sequence <= seq {
                return true;
            }
            sequences.shift_remove(&key);
            false
        }
        crate::types::ServiceProviderUpdate::Reset { snapshot } => {
            sequences.clear();
            record_snapshot_sequences(sequences, &snapshot.instances);
            false
        }
        crate::types::ServiceProviderUpdate::Replaced { snapshot } => {
            sequences.clear();
            record_snapshot_sequences(sequences, std::slice::from_ref(snapshot));
            false
        }
        crate::types::ServiceProviderUpdate::Spawned { instance } => {
            record_snapshot_sequences(sequences, std::slice::from_ref(instance));
            false
        }
        crate::types::ServiceProviderUpdate::Unavailable => {
            sequences.clear();
            false
        }
        crate::types::ServiceProviderUpdate::Closed { instance } => {
            let encoded = pi_js::json::stringify(&serde_json::json!([&instance.key, instance.generation]));
            let prefix = format!("{},", encoded.strip_suffix(']').unwrap_or(&encoded));
            sequences.retain(|key, _| !key.starts_with(&prefix));
            false
        }
    }
}

fn state_member_key(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    match instance {
        None => pi_js::json::stringify(&serde_json::json!([member])),
        Some(address) => pi_js::json::stringify(&serde_json::json!([&address.key, address.generation, member])),
    }
}

fn classify_remote_service_implementation(
    service_id: &str,
    implementation: &dyn RemoteServiceImplementation,
    object: Arc<dyn std::any::Any + Send + Sync>,
) -> pi_js::Result<ClassifiedRemoteServiceImplementation> {
    let members = classify_implementation_members(service_id, implementation, Some(&object))?;
    Ok(ClassifiedRemoteServiceImplementation { object, members })
}

fn classify_implementation_members(
    service_id: &str,
    implementation: &dyn RemoteServiceImplementation,
    object: Option<&Arc<dyn std::any::Any + Send + Sync>>,
) -> pi_js::Result<indexmap::IndexMap<String, RemoteServiceMember>> {
    if let Some(svc_obj) = implementation
        .as_service_object()
        .or_else(|| object.and_then(|obj| obj.downcast_ref::<ServiceObject>()))
    {
        return classify_service_object(service_id, svc_obj);
    }

    let sorted: BTreeMap<String, RemoteServiceMember> = implementation.members().into_iter().collect();
    if sorted.is_empty() {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Remote service {service_id} has no members"),
        ));
    }
    Ok(sorted.into_iter().collect())
}

fn classify_service_object(
    service_id: &str,
    svc_obj: &ServiceObject,
) -> pi_js::Result<indexmap::IndexMap<String, RemoteServiceMember>> {
    let is_plain_object = !svc_obj.is_callable()
        && svc_obj.as_context().is_none()
        && svc_obj.to_json().is_none_or(|json| json.is_object());
    let Some(mut keys) = svc_obj.keys().filter(|_| is_plain_object) else {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Remote service {service_id} implementation must be an object"),
        ));
    };

    keys.sort();
    let mut members = indexmap::IndexMap::new();
    for name in keys {
        let value = svc_obj.get_key(&name)?;
        match value {
            ServiceValue::Object(member_obj) => {
                if member_obj.is_callable() {
                    let this_obj = svc_obj.clone();
                    let func_obj = member_obj.clone();
                    let method: RemoteServiceMethod = Arc::new(move |args, context| {
                        let this_arg = this_obj.clone();
                        let callee = func_obj.clone();
                        Box::pin(async move {
                            let mut call_args: Vec<ServiceValue> = args.iter().map(ServiceValue::from_json).collect();
                            call_args.push(ServiceValue::Object(ServiceObject::from_context(context)));
                            let outcome = callee.apply(Some(&this_arg), &call_args)?;
                            outcome.resolve_json().await
                        })
                    });
                    members.insert(name, RemoteServiceMember::Method(method));
                } else if let Some(internals) = member_obj
                    .state_internals()
                    .or_else(|| get_replicated_state_internals(&member_obj))
                {
                    members.insert(name, RemoteServiceMember::State(internals));
                } else {
                    return Err(pi_js::Error::js(
                        "TypeError",
                        format!("Remote service member {service_id}.{name} is not remotely exposable"),
                    ));
                }
            }
            _ => {
                return Err(pi_js::Error::js(
                    "TypeError",
                    format!("Remote service member {service_id}.{name} is not remotely exposable"),
                ));
            }
        }
    }

    if members.is_empty() {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Remote service {service_id} has no members"),
        ));
    }
    Ok(members)
}

fn service_object_from_members(members: &indexmap::IndexMap<String, RemoteServiceMember>) -> ServiceObject {
    let mut map = IndexMap::new();
    for (name, member) in members {
        match member {
            RemoteServiceMember::Method(method) => {
                let method = Arc::clone(method);
                let fn_obj = ServiceObject::from_fn(move |_this, args| {
                    let (json_args, context) = split_method_args_and_context(args)?;
                    let fut = method(json_args, context);
                    Ok(ServiceValue::Object(ServiceObject::from_promise(fut)))
                });
                map.insert(name.clone(), ServiceValue::Object(fn_obj));
            }
            RemoteServiceMember::State(internals) => {
                let state_obj = ServiceObject::new(Arc::new(InternalsBackedStateTarget(Arc::clone(internals))));
                map.insert(name.clone(), ServiceValue::Object(state_obj));
            }
        }
    }
    ServiceObject::from_members(map)
}

fn split_method_args_and_context(
    args: &[ServiceValue],
) -> pi_js::Result<(Vec<crate::types::JsonValue>, crate::types::Context)> {
    let (arg_slice, context) = match args.last() {
        Some(ServiceValue::Object(obj)) if obj.as_context().is_some() => {
            (&args[..args.len() - 1], obj.as_context().unwrap())
        }
        _ => (args, service_delivery_context()),
    };
    let mut json_args = Vec::with_capacity(arg_slice.len());
    for arg in arg_slice {
        json_args.push(arg.to_json()?);
    }
    Ok((json_args, context))
}

struct InternalsBackedStateTarget(Arc<dyn ReplicatedStateInternals>);

impl ServiceTarget for InternalsBackedStateTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let PropertyKey::String(key) = property else {
            return Ok(ServiceValue::Undefined);
        };
        if key == "value" {
            return Ok(ServiceValue::from_json(&self.0.snapshot().value));
        }
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "ReplicatedState is not callable"))
    }

    fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        Some(Arc::clone(&self.0))
    }
}

fn mode_str(mode: crate::types::ServiceMode) -> &'static str {
    match mode {
        crate::types::ServiceMode::Singleton => "singleton",
        crate::types::ServiceMode::Keyed => "keyed",
    }
}

fn service_member_shape(
    members: &indexmap::IndexMap<String, RemoteServiceMember>,
) -> indexmap::IndexMap<String, ServiceMemberKind> {
    members
        .iter()
        .map(|(name, member)| {
            let kind = match member {
                RemoteServiceMember::Method(_) => ServiceMemberKind::Method,
                RemoteServiceMember::State(_) => ServiceMemberKind::State,
            };
            (name.clone(), kind)
        })
        .collect()
}

fn same_service_member_shape(
    left: &indexmap::IndexMap<String, ServiceMemberKind>,
    right: &indexmap::IndexMap<String, ServiceMemberKind>,
) -> bool {
    left.len() == right.len() && left.iter().all(|(name, kind)| right.get(name) == Some(kind))
}

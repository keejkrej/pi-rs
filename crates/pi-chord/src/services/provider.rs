//! Port of packages/chord/src/services/provider.ts

#![allow(dead_code, unused_variables)]

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use crate::services::state_internals::ReplicatedStateInternals;

static NEXT_PROVIDER_SUBSCRIBER_ID: AtomicU64 = AtomicU64::new(1);

/// `(subscriptionId, update, context) => void | Promise<void>`.
pub type ServiceUpdatePublisher = Arc<
    dyn Fn(
            String,
            crate::types::ServiceProviderUpdate,
            crate::types::Context,
        ) -> pi_js::BoxFuture< pi_js::Result<()>>
        + Send
        + Sync,
>;

/// `(...args, context) => Promise<JsonValue | void>`.
pub type RemoteServiceMethod = Arc<
    dyn Fn(
            Vec<crate::types::JsonValue>,
            crate::types::Context,
        ) -> pi_js::BoxFuture< pi_js::Result<Option<crate::types::JsonValue>>>
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
        todo!("port: RemoteServiceEndpoint::invoke")
    }

    pub fn dispose(&self) {
        todo!("port: RemoteServiceEndpoint::dispose")
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
    address: Option<crate::types::ServiceInstanceAddress>,
    implementation: Arc<dyn std::any::Any + Send + Sync>,
    members: indexmap::IndexMap<String, RemoteServiceMember>,
    remove_member_listeners: Vec<pi_js::Unsubscribe>,
    active: bool,
}

type ServiceProviderListener = Arc<dyn Fn(crate::types::ServiceProviderUpdate, crate::types::Context) + Send + Sync>;

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
        todo!("port: RemoteServiceSubscription::activate")
    }

    pub fn close(&self, context: Option<crate::types::Context>) -> pi_js::Result<()> {
        todo!("port: RemoteServiceSubscription::close")
    }
}

struct ClassifiedRemoteServiceImplementation {
    object: Arc<dyn std::any::Any + Send + Sync>,
    members: indexmap::IndexMap<String, RemoteServiceMember>,
}

impl RemoteServiceProvider {
    pub fn new(entries: Vec<RemoteServiceProviderEntry>) -> pi_js::Result<Self> {
        todo!("port: RemoteServiceProvider::new")
    }

    pub fn catalogue(&self) -> &[crate::types::ServiceCatalogueEntry] {
        &self.inner.catalogue
    }

    pub fn provide<T>(&self, service: &crate::types::Service, implementation: Arc<T>) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        todo!("port: RemoteServiceProvider::provide")
    }

    /// Disconnect one singleton while preserving active subscriptions and remote facades.
    pub fn withdraw(&self, service: &crate::types::Service) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::withdraw")
    }

    /// Check a singleton replacement without changing the active provider.
    pub fn validate_replacement<T>(&self, service: &crate::types::Service, implementation: &T) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        todo!("port: RemoteServiceProvider::validate_replacement")
    }

    /// Replace one singleton without making its stable remote facade unavailable.
    pub fn replace<T>(&self, service: &crate::types::Service, implementation: Arc<T>) -> pi_js::Result<()>
    where
        T: RemoteServiceImplementation,
    {
        todo!("port: RemoteServiceProvider::replace")
    }

    pub fn use_<T>(&self, service: &crate::types::Service) -> pi_js::Result<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: RemoteServiceProvider::use_")
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
        todo!("port: RemoteServiceProvider::spawn")
    }

    pub async fn invoke(
        &self,
        call: &crate::types::ServiceCall,
        context: crate::types::Context,
    ) -> pi_js::Result<Option<crate::types::JsonValue>> {
        todo!("port: RemoteServiceProvider::invoke")
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
        todo!("port: RemoteServiceProvider::subscribe")
    }

    pub fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::dispose")
    }

    fn registration(&self, service_id: &str, mode: crate::types::ServiceMode) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::registration")
    }

    fn create_instance(
        &self,
        service_id: &str,
        classified: ClassifiedRemoteServiceImplementation,
        address: Option<crate::types::ServiceInstanceAddress>,
    ) -> pi_js::Result<ProviderInstance> {
        todo!("port: RemoteServiceProvider::create_instance")
    }

    fn assert_singleton_shape(
        &self,
        service_id: &str,
        current: Option<&indexmap::IndexMap<String, ServiceMemberKind>>,
        replacement: &indexmap::IndexMap<String, ServiceMemberKind>,
    ) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::assert_singleton_shape")
    }

    fn resolve_instance(
        &self,
        service_id: &str,
        address: Option<&crate::types::ServiceInstanceAddress>,
    ) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::resolve_instance")
    }

    fn snapshot(&self, service_id: &str) -> pi_js::Result<crate::types::ServiceSubscriptionSnapshot> {
        todo!("port: RemoteServiceProvider::snapshot")
    }

    fn snapshot_instance(&self, instance: &ProviderInstance) -> crate::types::ServiceInstanceSnapshot {
        todo!("port: RemoteServiceProvider::snapshot_instance")
    }

    // Queue for everyone before invoking user code, including reentrant publications.
    fn emit(
        &self,
        service_id: &str,
        update: crate::types::ServiceProviderUpdate,
        context: Option<crate::types::Context>,
    ) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::emit")
    }

    fn assert_remotable(&self, service: &crate::types::Service) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::assert_remotable")
    }

    fn assert_allowed(&self, service_id: &str) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::assert_allowed")
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        todo!("port: RemoteServiceProvider::assert_active")
    }
}

pub fn create_remote_service_endpoint(provider: &RemoteServiceProvider) -> RemoteServiceEndpoint {
    todo!("port: create_remote_service_endpoint")
}

pub fn validate_remote_service_implementation(
    service_id: &str,
    implementation: &dyn RemoteServiceImplementation,
) -> pi_js::Result<()> {
    todo!("port: validate_remote_service_implementation")
}

fn drain_subscriber(subscriber: &ProviderSubscriber) -> Vec<pi_js::Error> {
    todo!("port: drain_subscriber")
}

fn record_snapshot_sequences(
    sequences: &mut indexmap::IndexMap<String, i64>,
    instances: &[crate::types::ServiceInstanceSnapshot],
) {
    todo!("port: record_snapshot_sequences")
}

fn update_covered_by_snapshot(
    sequences: &mut indexmap::IndexMap<String, i64>,
    update: &crate::types::ServiceProviderUpdate,
) -> bool {
    todo!("port: update_covered_by_snapshot")
}

fn state_member_key(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    todo!("port: state_member_key")
}

fn throw_collected_errors(errors: &[pi_js::Error], message: &str) -> pi_js::Result<()> {
    todo!("port: throw_collected_errors")
}

fn classify_remote_service_implementation(
    service_id: &str,
    implementation: &dyn RemoteServiceImplementation,
    object: Arc<dyn std::any::Any + Send + Sync>,
) -> pi_js::Result<ClassifiedRemoteServiceImplementation> {
    todo!("port: classify_remote_service_implementation")
}

fn service_member_shape(
    members: &indexmap::IndexMap<String, RemoteServiceMember>,
) -> indexmap::IndexMap<String, ServiceMemberKind> {
    todo!("port: service_member_shape")
}

fn same_service_member_shape(
    left: &indexmap::IndexMap<String, ServiceMemberKind>,
    right: &indexmap::IndexMap<String, ServiceMemberKind>,
) -> bool {
    todo!("port: same_service_member_shape")
}

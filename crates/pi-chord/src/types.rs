//! Port of packages/chord/src/types.ts
#![allow(dead_code, unused_variables)]

use std::any::TypeId;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::delta::{Draft, Op};

pub use crate::services::errors::RemoteServiceError;
pub use crate::services::provider::RemoteServiceProvider;

/// Strict JSON: null, bool, number, string, array, or object.
///
/// PORT: `serde_json::Value` with workspace `preserve_order`. Codecs call `pi_js::json` later.
pub type JsonValue = serde_json::Value;

/// Strict-JSON representation of an application data type. Unknown payloads become JsonValue.
///
/// PORT: TS `JsonRepresentation<T>` is a conditional type (`IsAny`, arrays, objects). Rust keeps `T`.
/// Callers that need strict JSON use [`JsonValue`].
pub type JsonRepresentation<T> = T;

/// PORT: TS `RemoteServiceContract<T>` rejects non-JSON members at compile time. Rust cannot express
/// that check; the alias is `T` and the provider validates when the bodies are filled.
pub type RemoteServiceContract<T> = T;

/// Identity of one [`ContextKey`]. JS `symbol` equality is object identity, modeled by [`ContextToken::id`].
#[derive(Clone, Debug)]
pub struct ContextToken {
    pub description: String,
    pub(crate) id: u64,
}

impl ContextToken {
    pub(crate) fn new(id: u64, description: impl Into<String>) -> Self {
        Self {
            id,
            description: description.into(),
        }
    }
}

impl PartialEq for ContextToken {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ContextToken {}

impl Hash for ContextToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Typed identity for one value carried by a [`Context`].
pub struct ContextKey<T> {
    pub token: ContextToken,
    /// Type-only marker that prevents keys with different value types from being interchangeable.
    _value_type: PhantomData<fn() -> T>,
}

impl<T> Clone for ContextKey<T> {
    fn clone(&self) -> Self {
        Self {
            token: self.token.clone(),
            _value_type: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for ContextKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextKey").field("token", &self.token).finish()
    }
}

impl<T> PartialEq for ContextKey<T> {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
    }
}

impl<T> Eq for ContextKey<T> {}

impl<T> Hash for ContextKey<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.token.hash(state);
    }
}

impl<T> ContextKey<T> {
    pub(crate) fn from_token(token: ContextToken) -> Self {
        Self {
            token,
            _value_type: PhantomData,
        }
    }
}

#[derive(Clone)]
enum ContextInner {
    Empty {
        name: &'static str,
    },
    Value {
        parent: Context,
        key_id: u64,
        type_id: TypeId,
        value: Arc<dyn std::any::Any + Send + Sync>,
        description: String,
    },
}

/// Immutable invocation-scoped values passed explicitly through operations.
#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

impl Context {
    pub(crate) fn empty(name: &'static str) -> Self {
        Self {
            inner: Arc::new(ContextInner::Empty { name }),
        }
    }

    /// PORT: stored values are `Clone + Send + Sync + 'static` so they can live in `Any` and be returned by clone.
    pub fn value<T>(&self, key: &ContextKey<T>) -> Option<T>
    where
        T: Clone + Send + Sync + 'static,
    {
        todo!("port: Context::value")
    }

    pub fn abort_signal(&self) -> Option<pi_js::abort::AbortSignal> {
        todo!("port: Context::abort_signal")
    }
}

impl std::fmt::Display for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("port: Context::fmt")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicatedStateDeliveryKind {
    #[serde(rename = "hydrate")]
    Hydrate,
    #[serde(rename = "update")]
    Update,
}

/// In-process delivery tag. Key order is `kind`, `sequence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicatedStateDelivery {
    pub kind: ReplicatedStateDeliveryKind,
    pub sequence: i64,
}

/// Contract-immutable value, or undefined until hydration.
///
/// Each subscription serializes callbacks, awaiting hydration before updates. Values are immutable and may
/// share unchanged revision data. At most 100 deliveries wait behind the running callback; overflow keeps
/// only the newest pending value/context/delivery, so update sequences may skip. Failures are reported in
/// isolation and delivery continues. Unsubscribe discards pending work without aborting or joining a callback.
///
/// PORT: sync and promise listeners are one callback returning [`pi_js::BoxFuture`]. This handle does not
/// inherit into [`MutableReplicatedState`] or [`AttachedReplicatedState`]; those narrow `value` to `T`.
#[derive(Clone)]
pub struct ReplicatedState<T = JsonValue> {
    inner: Arc<ReplicatedStateInner<T>>,
}

struct ReplicatedStateInner<T> {
    _value: PhantomData<T>,
}

impl<T> ReplicatedState<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn value(&self) -> Option<T> {
        todo!("port: ReplicatedState::value")
    }

    pub fn subscribe(
        &self,
        listener: Arc<dyn Fn(T, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        todo!("port: ReplicatedState::subscribe")
    }
}

/// One immutable authoritative revision committed after an attachment snapshot.
///
/// Not JSON ([`Context`] is in-process). Field order is the interface order: `cursor`, `value`, `ops`, `context`.
#[derive(Clone)]
pub struct ReplicatedStateSourceFrame<T = JsonValue> {
    /// Monotonic source cursor. The first frame after a snapshot must be `snapshot.cursor + 1`.
    pub cursor: i64,
    /// The exact immutable value produced by this commit.
    pub value: T,
    /// The exact immutable operation batch that produced `value` from the preceding source revision.
    pub ops: Vec<Op>,
    pub context: Context,
}

/// Fixed immutable snapshot captured at the atomic attachment boundary.
///
/// PORT: TS inlines `{ readonly value: T; readonly cursor: number }`. Field order is `value`, `cursor`.
#[derive(Clone, Debug)]
pub struct ReplicatedStateSourceSnapshot<T = JsonValue> {
    pub value: T,
    pub cursor: i64,
}

/// Attachment returned by [`ReplicatedStateSource::attach`].
///
/// PORT: object-safe so `state.rs` can store `Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>`.
/// `activate` takes one `Arc<dyn Fn>` rather than a generic closure.
pub trait ReplicatedStateSourceAttachment<T = JsonValue>: Send + Sync {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<T>;

    /// Install the sole listener and synchronously drain every buffered frame in source commit order.
    /// This method is single-use. After it begins, every new committed frame must also be delivered in order
    /// until disposal, including commits made reentrantly while a prior frame is being delivered.
    fn activate(
        &self,
        listener: Arc<dyn Fn(ReplicatedStateSourceFrame<T>) -> pi_js::Result<()> + Send + Sync>,
    ) -> pi_js::Result<()>;

    /// Stop delivery and release source resources. Disposal must be idempotent.
    fn dispose(&self) -> pi_js::Result<()>;
}

/// An authoritative immutable revision source.
///
/// `attach()` must synchronously and atomically capture one snapshot and register the returned attachment to buffer
/// every later committed frame. The snapshot must include every commit before that boundary; buffered frames must
/// include every commit after it, with no overlap or gap. Snapshot values, frame values, and operation batches are
/// immutable and remain valid after delivery. Chord only publishes these references; it never applies or re-diffs them.
///
/// PORT: a trait, not a handle. `T` defaults to [`JsonValue`]. `api.rs` names `ReplicatedStateSource` in value position.
pub trait ReplicatedStateSource<T = JsonValue>: Send + Sync {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>>;
}

pub struct ReplicatedStateSourceOptions {
    /// Receives source-contract and publication-listener failures without throwing them into the source.
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

impl Clone for ReplicatedStateSourceOptions {
    fn clone(&self) -> Self {
        Self {
            on_error: self.on_error.clone(),
        }
    }
}

/// A synchronously hydrated publication-only state backed by one source attachment.
///
/// PORT: separate from [`ReplicatedState`] so `value` is `T`, not `T | undefined`.
#[derive(Clone)]
pub struct AttachedReplicatedState<T = JsonValue> {
    inner: Arc<AttachedReplicatedStateInner<T>>,
}

struct AttachedReplicatedStateInner<T> {
    _value: PhantomData<T>,
}

impl<T> AttachedReplicatedState<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn value(&self) -> T {
        todo!("port: AttachedReplicatedState::value")
    }

    pub fn subscribe(
        &self,
        listener: Arc<dyn Fn(T, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        todo!("port: AttachedReplicatedState::subscribe")
    }

    /// Idempotently release the source attachment. The last published value remains readable.
    pub fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: AttachedReplicatedState::dispose")
    }
}

/// Mutable replicated state. `value` is always present.
///
/// `change` atomically publishes one synchronous overlay mutation. Draft handles are unusable after the callback
/// returns. Values placed through the draft are cloned by value; assigning `undefined` to an object property deletes it.
/// `replace` atomically takes immutable ownership of an alias-free strict-JSON replacement.
///
/// PORT: separate from [`ReplicatedState`] so `value` returns `T`. `change` takes `FnOnce` because the callback runs once.
#[derive(Clone)]
pub struct MutableReplicatedState<T = JsonValue> {
    inner: Arc<MutableReplicatedStateInner<T>>,
}

struct MutableReplicatedStateInner<T> {
    _value: PhantomData<T>,
}

impl<T> MutableReplicatedState<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn value(&self) -> T {
        todo!("port: MutableReplicatedState::value")
    }

    pub fn subscribe(
        &self,
        listener: Arc<dyn Fn(T, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        todo!("port: MutableReplicatedState::subscribe")
    }

    pub fn change<F>(&self, context: Context, mutate: F) -> pi_js::Result<()>
    where
        F: FnOnce(&mut Draft<T>) + Send,
    {
        todo!("port: MutableReplicatedState::change")
    }

    pub fn replace(&self, context: Context, value: T) -> pi_js::Result<()> {
        todo!("port: MutableReplicatedState::replace")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceMode {
    #[serde(rename = "singleton")]
    Singleton,
    #[serde(rename = "keyed")]
    Keyed,
}

/// Stable identity for one shared service contract.
///
/// PORT: TS `Service<T>` carries a type-only unique symbol. Every other module passes one concrete token
/// (`id`, `local`), so `T` is not a Rust parameter. Process-local services accept unrestricted object contracts
/// and are never published remotely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub id: String,
    pub local: bool,
}

impl Service {
    pub fn new(id: impl Into<String>, local: bool) -> Self {
        Self { id: id.into(), local }
    }
}

/// PORT: TS `ServiceSpawner<T>.spawn` returns an unsubscribe function.
#[derive(Clone)]
pub struct ServiceSpawner<T = JsonValue> {
    inner: Arc<ServiceSpawnerInner<T>>,
}

struct ServiceSpawnerInner<T> {
    _implementation: PhantomData<T>,
}

impl<T> ServiceSpawner<T>
where
    T: Send + Sync + 'static,
{
    pub fn spawn(&self, key: &str, implementation: T) -> pi_js::Unsubscribe {
        todo!("port: ServiceSpawner::spawn")
    }
}

/// Acquired remote services.
///
/// PORT: a handle, not a trait. [`RemoteServiceBinding`] repeats these methods instead of inheriting them.
/// `observe` is the promise-listener form of the TS overload pair.
#[derive(Clone)]
pub struct RemoteServices {
    inner: Arc<RemoteServicesInner>,
}

struct RemoteServicesInner {
    _private: (),
}

impl RemoteServices {
    pub fn r#use<T>(&self, service: &Service) -> pi_js::Result<T>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: RemoteServices::use")
    }

    pub fn observe<T>(
        &self,
        service: &Service,
        handler: Arc<dyn Fn(T, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Result<pi_js::Unsubscribe>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: RemoteServices::observe")
    }

    /// Wait until every currently acquired service has installed its initial snapshot.
    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServices::ready")
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServices::dispose")
    }
}

/// Key order is `serviceId`, `mode`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCatalogueEntry {
    pub service_id: String,
    pub mode: ServiceMode,
}

/// Key order is `key`, `generation`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInstanceAddress {
    pub key: String,
    pub generation: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceMemberMethodKind {
    #[serde(rename = "method")]
    Method,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceMemberStateKind {
    #[serde(rename = "state")]
    State,
}

/// Method arm. Key order is `name`, `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceMethodSnapshot {
    pub name: String,
    pub kind: ServiceMemberMethodKind,
}

/// State arm. Key order is `name`, `kind`, `sequence`, `ops`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStateSnapshot {
    pub name: String,
    pub kind: ServiceMemberStateKind,
    pub sequence: i64,
    pub ops: Vec<Op>,
}

/// PORT: writers emit `name` before `kind`. An internal tag would emit `kind` first, so the union is untagged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum ServiceMemberSnapshot {
    Method(ServiceMethodSnapshot),
    State(ServiceStateSnapshot),
}

/// Key order is `instance`, `members` (`instance` omitted when absent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInstanceSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<ServiceInstanceAddress>,
    pub members: Vec<ServiceMemberSnapshot>,
}

/// Key order is `serviceId`, `mode`, `instances`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSubscriptionSnapshot {
    pub service_id: String,
    pub mode: ServiceMode,
    pub instances: Vec<ServiceInstanceSnapshot>,
}

/// Key order follows the object literals in `provider.ts`. `type` is first, then the remaining keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServiceProviderUpdate {
    #[serde(rename = "state")]
    State {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance: Option<ServiceInstanceAddress>,
        member: String,
        sequence: i64,
        ops: Vec<Op>,
    },
    /// Full subscription rebaseline after overflow; every state is a root replacement at its new sequence.
    #[serde(rename = "reset")]
    Reset { snapshot: ServiceSubscriptionSnapshot },
    #[serde(rename = "unavailable")]
    Unavailable,
    #[serde(rename = "replaced")]
    Replaced { snapshot: ServiceInstanceSnapshot },
    #[serde(rename = "spawned")]
    Spawned { instance: ServiceInstanceSnapshot },
    #[serde(rename = "closed")]
    Closed { instance: ServiceInstanceAddress },
}

/// Key order is `serviceId`, `instance`, `member`, `args`. Absent `instance` is omitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCall {
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<ServiceInstanceAddress>,
    pub member: String,
    /// Borrowed immutable values. Chord validates but does not clone them.
    pub args: Vec<JsonValue>,
}

struct ServiceSubscriptionInner {
    snapshot: ServiceSubscriptionSnapshot,
    activate: Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>,
    close: Arc<dyn Fn(Option<Context>) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
}

/// Atomic baseline. Later updates buffer until activation, with a full reset on pending delivery 101.
///
/// PORT: `close` is async because TS returns `void | Promise<void>`. `context` is optional.
#[derive(Clone)]
pub struct ServiceSubscription {
    inner: Arc<ServiceSubscriptionInner>,
}

impl ServiceSubscription {
    /// PORT: TS object literal `{ snapshot, activate, close }`.
    pub fn new(
        snapshot: ServiceSubscriptionSnapshot,
        activate: Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>,
        close: Arc<dyn Fn(Option<Context>) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(ServiceSubscriptionInner {
                snapshot,
                activate,
                close,
            }),
        }
    }

    pub fn snapshot(&self) -> &ServiceSubscriptionSnapshot {
        &self.inner.snapshot
    }

    pub fn activate(&self) -> pi_js::Result<()> {
        todo!("port: ServiceSubscription::activate")
    }

    pub async fn close(&self, context: Option<Context>) -> pi_js::Result<()> {
        todo!("port: ServiceSubscription::close")
    }
}

/// Pluggable wire boundary consumed by a remote service binding.
///
/// Implementations choose transport, framing, routing, and envelope encoding. Values crossing this
/// boundary must remain strict JSON. Chord does not clone values or require a particular application wire protocol;
/// adapters own serialization and any isolation copies they require.
#[derive(Clone)]
pub struct RemoteServiceTransport {
    inner: Arc<RemoteServiceTransportInner>,
}

struct RemoteServiceTransportInner {
    invoke: Arc<dyn Fn(&ServiceCall, &Context) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync>,
    subscribe: Arc<
        dyn Fn(
                &str,
                ServiceMode,
                Arc<dyn Fn(ServiceProviderUpdate, Context) + Send + Sync>,
                &Context,
            ) -> pi_js::BoxFuture<pi_js::Result<ServiceSubscription>>
            + Send
            + Sync,
    >,
}

impl RemoteServiceTransport {
    /// PORT: TS object literal `{ invoke, subscribe }`.
    pub fn new(
        invoke: Arc<dyn Fn(&ServiceCall, &Context) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync>,
        subscribe: Arc<
            dyn Fn(
                    &str,
                    ServiceMode,
                    Arc<dyn Fn(ServiceProviderUpdate, Context) + Send + Sync>,
                    &Context,
                ) -> pi_js::BoxFuture<pi_js::Result<ServiceSubscription>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self {
            inner: Arc::new(RemoteServiceTransportInner { invoke, subscribe }),
        }
    }

    pub async fn invoke(&self, call: &ServiceCall, context: &Context) -> pi_js::Result<Option<JsonValue>> {
        todo!("port: RemoteServiceTransport::invoke")
    }

    pub async fn subscribe(
        &self,
        service_id: &str,
        mode: ServiceMode,
        listener: Arc<dyn Fn(ServiceProviderUpdate, Context) + Send + Sync>,
        context: &Context,
    ) -> pi_js::Result<ServiceSubscription> {
        todo!("port: RemoteServiceTransport::subscribe")
    }
}

/// PORT: TS inlines `{ readonly id: string }` on binding options and `RemoteServiceSource.open`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceIdRef {
    pub id: String,
}

/// PORT: TS inlines the `open` options object. `assertAccess` and `onError` are required there.
pub struct RemoteServiceSourceOpenOptions {
    pub services: Vec<ServiceIdRef>,
    pub assert_access: Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>,
    pub on_error: Arc<dyn Fn(pi_js::Error) + Send + Sync>,
}

pub struct RemoteServiceBindingOptions {
    pub services: Vec<ServiceIdRef>,
    pub transport: RemoteServiceTransport,
    pub bound: Option<bool>,
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
    pub assert_access: Option<Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>>,
}

/// Binding over a [`RemoteServiceTransport`]. Adds [`RemoteServiceBinding::rebind`] to the [`RemoteServices`] methods.
#[derive(Clone)]
pub struct RemoteServiceBinding {
    inner: Arc<RemoteServiceBindingInner>,
}

struct RemoteServiceBindingInner {
    _private: (),
}

impl RemoteServiceBinding {
    pub fn r#use<T>(&self, service: &Service) -> pi_js::Result<T>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: RemoteServiceBinding::use")
    }

    pub fn observe<T>(
        &self,
        service: &Service,
        handler: Arc<dyn Fn(T, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Result<pi_js::Unsubscribe>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: RemoteServiceBinding::observe")
    }

    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBinding::ready")
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBinding::dispose")
    }

    pub async fn rebind(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBinding::rebind")
    }
}

struct RemoteServiceSourceInner {
    accepts_unavailable_services: bool,
    catalogue: Arc<dyn Fn(Context) -> pi_js::BoxFuture<pi_js::Result<Vec<ServiceCatalogueEntry>>> + Send + Sync>,
    open: Arc<dyn Fn(RemoteServiceSourceOpenOptions) -> RemoteServices + Send + Sync>,
}

/// Source of remote service catalogues.
///
/// PORT: `Eq + Hash` compare `Arc` identity so `host.rs` can use this as a JS `Map` key.
#[derive(Clone)]
pub struct RemoteServiceSource {
    inner: Arc<RemoteServiceSourceInner>,
}

impl PartialEq for RemoteServiceSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for RemoteServiceSource {}

impl Hash for RemoteServiceSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.inner), state);
    }
}

impl RemoteServiceSource {
    /// PORT: TS object literal implementing `RemoteServiceSource`.
    pub fn new(
        accepts_unavailable_services: bool,
        catalogue: Arc<dyn Fn(Context) -> pi_js::BoxFuture<pi_js::Result<Vec<ServiceCatalogueEntry>>> + Send + Sync>,
        open: Arc<dyn Fn(RemoteServiceSourceOpenOptions) -> RemoteServices + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(RemoteServiceSourceInner {
                accepts_unavailable_services,
                catalogue,
                open,
            }),
        }
    }

    /// Whether this currently unavailable source may provisionally own absent requirements.
    pub fn accepts_unavailable_services(&self) -> bool {
        self.inner.accepts_unavailable_services
    }

    pub fn catalogue(&self, context: Context) -> pi_js::BoxFuture<pi_js::Result<Vec<ServiceCatalogueEntry>>> {
        todo!("port: RemoteServiceSource::catalogue")
    }

    pub fn open(&self, options: RemoteServiceSourceOpenOptions) -> RemoteServices {
        todo!("port: RemoteServiceSource::open")
    }
}

type Disposal = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

/// Environment passed to [`Facet::setup`]. Generic methods stay on the struct.
#[derive(Clone)]
pub struct FacetEnvironment {
    inner: Arc<FacetEnvironmentInner>,
}

struct FacetEnvironmentInner {
    _private: (),
}

impl FacetEnvironment {
    /// Declare a hard dependency on one singleton service and return its stable handle.
    pub fn r#use<T>(&self, service: &Service) -> pi_js::Result<T>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: FacetEnvironment::use")
    }

    /// Declare a hard dependency on a keyed service and observe each live instance.
    ///
    /// PORT: TS returns `void` here (no unsubscribe), unlike [`RemoteServices::observe`].
    pub fn observe<T>(
        &self,
        service: &Service,
        handler: Arc<dyn Fn(T, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> pi_js::Result<()>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: FacetEnvironment::observe")
    }

    /// Declare and install this facet's singleton implementation of a service.
    pub fn provide<T>(&self, service: &Service, implementation: T) -> pi_js::Result<()>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: FacetEnvironment::provide")
    }

    /// Declare ownership of a multi-instance service and return its deferred spawning capability.
    pub fn provide_many<T>(&self, service: &Service) -> pi_js::Result<ServiceSpawner<T>>
    where
        T: Send + Sync + 'static,
    {
        todo!("port: FacetEnvironment::provide_many")
    }

    /// Create initialized mutable state by taking immutable ownership of an alias-free strict-JSON root.
    /// The caller must not mutate `initial` after this call.
    ///
    /// PORT: TS constrains `T extends object`. Rust checks that when the body is filled.
    pub fn replicated_state<T>(&self, initial: T) -> pi_js::Result<MutableReplicatedState<T>>
    where
        T: Clone + Send + Sync + 'static,
    {
        todo!("port: FacetEnvironment::replicated_state")
    }

    /// Give the facet ownership of a resource cleanup function.
    pub fn own(&self, disposal: Disposal) -> pi_js::Result<()> {
        todo!("port: FacetEnvironment::own")
    }

    /// Register asynchronous initialization after dependencies are bound and ready.
    pub fn on_activate(&self, callback: Disposal) -> pi_js::Result<()> {
        todo!("port: FacetEnvironment::on_activate")
    }

    /// Register final facet teardown.
    pub fn on_deactivate(&self, callback: Disposal) -> pi_js::Result<()> {
        todo!("port: FacetEnvironment::on_deactivate")
    }
}

struct FacetInner {
    id: String,
    setup: Arc<dyn Fn(&FacetEnvironment) -> pi_js::Result<()> + Send + Sync>,
}

#[derive(Clone)]
pub struct Facet {
    inner: Arc<FacetInner>,
}

impl Facet {
    /// PORT: TS object literal `{ id, setup }`.
    pub fn new(
        id: impl Into<String>,
        setup: Arc<dyn Fn(&FacetEnvironment) -> pi_js::Result<()> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(FacetInner { id: id.into(), setup }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn setup(&self, env: &FacetEnvironment) -> pi_js::Result<()> {
        todo!("port: Facet::setup")
    }
}

pub struct FacetOptions {
    pub facets: Vec<Facet>,
    pub service_sources: Option<Vec<RemoteServiceSource>>,
    pub on_error: Option<Arc<dyn Fn(pi_js::Error) + Send + Sync>>,
}

/// Active facet host.
#[derive(Clone)]
pub struct FacetHost {
    inner: Arc<FacetHostInner>,
}

struct FacetHostInner {
    _private: (),
}

impl FacetHost {
    pub fn services(&self) -> RemoteServiceProvider {
        todo!("port: FacetHost::services")
    }

    /// Activate and replace facets with matching IDs without disconnecting consumer service handles.
    pub async fn reload(&self, facets: Vec<Facet>) -> pi_js::Result<()> {
        todo!("port: FacetHost::reload")
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: FacetHost::dispose")
    }
}

struct LoadedFacetsInner {
    facets: Vec<Facet>,
    dispose: Disposal,
}

#[derive(Clone)]
pub struct LoadedFacets {
    inner: Arc<LoadedFacetsInner>,
}

impl LoadedFacets {
    /// PORT: TS object literal `{ facets, dispose }`.
    pub fn new(facets: Vec<Facet>, dispose: Disposal) -> Self {
        Self {
            inner: Arc::new(LoadedFacetsInner { facets, dispose }),
        }
    }

    pub fn facets(&self) -> &[Facet] {
        &self.inner.facets
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: LoadedFacets::dispose")
    }
}

#[derive(Clone)]
pub struct FacetLoader {
    inner: Arc<FacetLoaderInner>,
}

struct FacetLoaderInner {
    load: Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<LoadedFacets>> + Send + Sync>,
}

impl FacetLoader {
    /// PORT: TS object literal `{ load }`.
    pub fn new(load: Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<LoadedFacets>> + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(FacetLoaderInner { load }),
        }
    }

    pub async fn load(&self) -> pi_js::Result<LoadedFacets> {
        todo!("port: FacetLoader::load")
    }
}

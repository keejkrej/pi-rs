//! Port of packages/chord/src/services/state.ts

#![allow(dead_code, unused_variables)]

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use crate::services::state_internals::{
    ReplicatedStateInternals, ReplicatedStateInternalsSnapshot, ReplicatedStateOpsListener,
};

static NEXT_STATE_LISTENER_ID: AtomicU64 = AtomicU64::new(1);

/// The void signature also accepts synchronous callbacks that return an ignored value.
///
/// Listeners are futures so delivery can await a thenable before the next frame, matching
/// `StateSubscriber.drain`. A synchronous listener returns an immediately ready future.
type StateListener<T> = Arc<
    dyn Fn(
            T,
            crate::types::Context,
            crate::types::ReplicatedStateDelivery,
        ) -> pi_js::BoxFuture< pi_js::Result<()>>
        + Send
        + Sync,
>;

type StateErrorReporter = Arc<dyn Fn(pi_js::Error) + Send + Sync>;

struct StateDelivery<T> {
    value: T,
    context: crate::types::Context,
    delivery: crate::types::ReplicatedStateDelivery,
}

struct StateSubscriberState<T> {
    pending: Vec<StateDelivery<T>>,
    running: bool,
    started: bool,
    closed: bool,
}

/// One public subscription, independent of producer and other subscriber progress.
#[derive(Clone)]
struct StateSubscriber<T: Send + Sync + 'static> {
    inner: Arc<StateSubscriberInner<T>>,
}

struct StateSubscriberInner<T: Send + Sync + 'static> {
    id: u64,
    listener: StateListener<T>,
    report_error: StateErrorReporter,
    state: Mutex<StateSubscriberState<T>>,
}

impl<T: Send + Sync + 'static> StateSubscriber<T> {
    fn new(listener: StateListener<T>, report_error: StateErrorReporter) -> Self {
        todo!("port: StateSubscriber::new")
    }

    // A cold replica can queue updates reentrantly before this subscriber's first hydration starts.
    fn push(&self, frame: StateDelivery<T>) {
        todo!("port: StateSubscriber::push")
    }

    fn drain(&self) {
        todo!("port: StateSubscriber::drain")
    }

    fn clear(&self) {
        todo!("port: StateSubscriber::clear")
    }

    fn close(&self) {
        todo!("port: StateSubscriber::close")
    }

    fn resume(&self) {
        todo!("port: StateSubscriber::resume")
    }

    fn report(&self, error: pi_js::Error) {
        todo!("port: StateSubscriber::report")
    }
}

struct Publication<T> {
    value: T,
    ops: Vec<crate::delta::Op>,
    sequence: i64,
    context: crate::types::Context,
}

struct PublisherSnapshot<T> {
    value: T,
    sequence: i64,
}

struct ReplicatedStatePublisherState<T: Send + Sync + 'static> {
    listeners: indexmap::IndexMap<u64, (StateSubscriber<T>, i64)>,
    source_listeners: indexmap::IndexMap<u64, ReplicatedStateOpsListener>,
    publications: Vec<Publication<T>>,
    value: T,
    sequence: i64,
    delivering: bool,
}

/// Maintains local publication order independently of how revisions are produced.
#[derive(Clone)]
struct ReplicatedStatePublisher<T: Send + Sync + 'static> {
    inner: Arc<ReplicatedStatePublisherInner<T>>,
}

struct ReplicatedStatePublisherInner<T: Send + Sync + 'static> {
    report_error: StateErrorReporter,
    state: Mutex<ReplicatedStatePublisherState<T>>,
}

impl<T: Send + Sync + 'static> ReplicatedStatePublisher<T> {
    fn new(initial: T, report_error: Option<StateErrorReporter>) -> Self {
        todo!("port: ReplicatedStatePublisher::new")
    }

    fn value(&self) -> T
    where
        T: Clone,
    {
        todo!("port: ReplicatedStatePublisher::value")
    }

    fn snapshot(&self) -> PublisherSnapshot<T>
    where
        T: Clone,
    {
        todo!("port: ReplicatedStatePublisher::snapshot")
    }

    fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(
                T,
                crate::types::Context,
                crate::types::ReplicatedStateDelivery,
            ) -> pi_js::BoxFuture< pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        todo!("port: ReplicatedStatePublisher::subscribe")
    }

    fn subscribe_source<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(&[crate::delta::Op], i64, crate::types::Context) + Send + Sync + 'static,
    {
        todo!("port: ReplicatedStatePublisher::subscribe_source")
    }

    /// Publish an already-prepared immutable revision and return isolated listener failures.
    fn publish(&self, value: T, ops: &[crate::delta::Op], context: crate::types::Context) -> Vec<pi_js::Error> {
        todo!("port: ReplicatedStatePublisher::publish")
    }
}

struct PublisherSource<T: Send + Sync + 'static> {
    publisher: ReplicatedStatePublisher<T>,
}

impl<T: Send + Sync + Clone + 'static> ReplicatedStateInternals for PublisherSource<T> {
    fn snapshot(&self) -> ReplicatedStateInternalsSnapshot {
        todo!("port: PublisherSource::snapshot")
    }

    fn subscribe(&self, listener: ReplicatedStateOpsListener) -> pi_js::Unsubscribe {
        todo!("port: PublisherSource::subscribe")
    }
}

#[derive(Clone)]
pub struct MutableReplicatedStateImpl<T: Send + Sync + 'static> {
    inner: Arc<MutableReplicatedStateImplInner<T>>,
}

struct MutableReplicatedStateImplInner<T: Send + Sync + 'static> {
    tracker: crate::delta::tracker::Tracker<T>,
    publisher: ReplicatedStatePublisher<T>,
    changing: Mutex<bool>,
}

impl<T: Send + Sync + 'static> MutableReplicatedStateImpl<T> {
    pub fn new(initial: T) -> pi_js::Result<Self> {
        todo!("port: MutableReplicatedStateImpl::new")
    }

    pub fn value(&self) -> T
    where
        T: Clone,
    {
        todo!("port: MutableReplicatedStateImpl::value")
    }

    pub fn change<F>(&self, context: crate::types::Context, mutate: F) -> pi_js::Result<()>
    where
        F: FnOnce(&mut crate::delta::draft::Draft<T>),
    {
        todo!("port: MutableReplicatedStateImpl::change")
    }

    pub fn replace(&self, context: crate::types::Context, value: T) -> pi_js::Result<()> {
        todo!("port: MutableReplicatedStateImpl::replace")
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(
                T,
                crate::types::Context,
                crate::types::ReplicatedStateDelivery,
            ) -> pi_js::BoxFuture< pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        todo!("port: MutableReplicatedStateImpl::subscribe")
    }
}

type BoxedSourceAttachment<T> = Box<dyn crate::types::ReplicatedStateSourceAttachment<T> + Send + Sync>;

struct AttachedRuntime {
    cursor: i64,
    disposed: bool,
}

#[derive(Clone)]
pub struct AttachedReplicatedStateImpl<T: Send + Sync + 'static> {
    inner: Arc<AttachedReplicatedStateImplInner<T>>,
}

struct AttachedReplicatedStateImplInner<T: Send + Sync + 'static> {
    publisher: ReplicatedStatePublisher<T>,
    attachment: BoxedSourceAttachment<T>,
    report_error: StateErrorReporter,
    runtime: Mutex<AttachedRuntime>,
}

impl<T: Send + Sync + 'static> AttachedReplicatedStateImpl<T> {
    fn new(
        attachment: BoxedSourceAttachment<T>,
        options: Option<crate::types::ReplicatedStateSourceOptions>,
    ) -> pi_js::Result<Self> {
        todo!("port: AttachedReplicatedStateImpl::new")
    }

    pub fn value(&self) -> T
    where
        T: Clone,
    {
        todo!("port: AttachedReplicatedStateImpl::value")
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(
                T,
                crate::types::Context,
                crate::types::ReplicatedStateDelivery,
            ) -> pi_js::BoxFuture< pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        todo!("port: AttachedReplicatedStateImpl::subscribe")
    }

    pub fn activate(&self) -> pi_js::Result<()> {
        todo!("port: AttachedReplicatedStateImpl::activate")
    }

    pub fn dispose(&self) -> pi_js::Result<()> {
        todo!("port: AttachedReplicatedStateImpl::dispose")
    }

    fn receive(&self, frame: crate::types::ReplicatedStateSourceFrame<T>) {
        todo!("port: AttachedReplicatedStateImpl::receive")
    }

    fn fail(&self, error: pi_js::Error) {
        todo!("port: AttachedReplicatedStateImpl::fail")
    }

    fn report(&self, error: pi_js::Error) {
        todo!("port: AttachedReplicatedStateImpl::report")
    }
}

/// Attach a publication-only replicated state to one authoritative immutable source stream.
pub fn attach_replicated_state_source<T, S>(
    source: S,
    options: Option<crate::types::ReplicatedStateSourceOptions>,
) -> pi_js::Result<AttachedReplicatedStateImpl<T>>
where
    T: Send + Sync + 'static,
    S: crate::types::ReplicatedStateSource<T> + Send + 'static,
{
    todo!("port: attach_replicated_state_source")
}

/// A cold read-only state used by service consumers until a complete snapshot arrives.
#[derive(Clone)]
pub struct ReplicatedStateReplica<T: Send + Sync + 'static = crate::types::JsonValue> {
    inner: Arc<ReplicatedStateReplicaInner<T>>,
}

struct ReplicaState<T: Send + Sync + 'static> {
    listeners: indexmap::IndexMap<u64, StateSubscriber<T>>,
    value: Option<T>,
    sequence: Option<i64>,
}

struct ReplicatedStateReplicaInner<T: Send + Sync + 'static> {
    report_error: StateErrorReporter,
    validator: crate::delta::revision_validator::JsonRevisionValidator,
    state: Mutex<ReplicaState<T>>,
}

impl<T: Send + Sync + 'static> ReplicatedStateReplica<T> {
    pub fn new(report_error: impl Fn(pi_js::Error) + Send + Sync + 'static) -> Self {
        todo!("port: ReplicatedStateReplica::new")
    }

    pub fn value(&self) -> Option<T>
    where
        T: Clone,
    {
        todo!("port: ReplicatedStateReplica::value")
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(
                T,
                crate::types::Context,
                crate::types::ReplicatedStateDelivery,
            ) -> pi_js::BoxFuture< pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        todo!("port: ReplicatedStateReplica::subscribe")
    }

    pub fn hydrate(
        &self,
        sequence: i64,
        ops: &[crate::delta::Op],
        context: crate::types::Context,
    ) -> pi_js::Result<()> {
        todo!("port: ReplicatedStateReplica::hydrate")
    }

    pub fn update(&self, sequence: i64, ops: &[crate::delta::Op], context: crate::types::Context) -> pi_js::Result<()> {
        todo!("port: ReplicatedStateReplica::update")
    }

    pub fn clear(&self) {
        todo!("port: ReplicatedStateReplica::clear")
    }

    // Enqueue for everyone before user code can publish another revision reentrantly.
    fn deliver_all(&self, context: crate::types::Context, delivery: crate::types::ReplicatedStateDelivery) {
        todo!("port: ReplicatedStateReplica::deliver_all")
    }
}

/// @internal Context for synthetic service deliveries without a caller.
// TODO: Add delivery-scoped cancellation or metadata if deliveries gain an owned lifecycle.
pub fn service_delivery_context() -> crate::types::Context {
    todo!("port: service_delivery_context")
}

enum CursorKind {
    Snapshot,
    Frame,
}

fn assert_cursor(cursor: i64, kind: CursorKind) -> pi_js::Result<()> {
    todo!("port: assert_cursor")
}

fn is_promise_like(value: &dyn std::any::Any) -> bool {
    todo!("port: is_promise_like")
}

fn throw_collected_errors(errors: &[pi_js::Error], message: &str) -> pi_js::Result<()> {
    todo!("port: throw_collected_errors")
}

fn report_error_async(error: pi_js::Error) {
    todo!("port: report_error_async")
}

fn to_error(error: pi_js::Error) -> pi_js::Error {
    todo!("port: to_error")
}

//! Port of packages/chord/src/services/state.ts

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::services::errors::{AggregateError, clone_error};
use crate::services::state_internals::{
    ReplicatedStateFallibleOpsListener, ReplicatedStateInternals, ReplicatedStateInternalsSnapshot,
    ReplicatedStateOpsListener, register_replicated_state_internals,
};

static NEXT_STATE_LISTENER_ID: AtomicU64 = AtomicU64::new(1);

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The void signature also accepts synchronous callbacks that return an ignored value.
///
/// Listeners are futures so delivery can await a thenable before the next frame, matching
/// `StateSubscriber.drain`. A synchronous listener returns an immediately ready future.
type StateListener<T> = Arc<
    dyn Fn(T, crate::types::Context, crate::types::ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
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
struct StateSubscriber<T: Send + Sync + 'static> {
    inner: Arc<StateSubscriberInner<T>>,
}

impl<T: Send + Sync + 'static> Clone for StateSubscriber<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct StateSubscriberInner<T: Send + Sync + 'static> {
    id: u64,
    listener: StateListener<T>,
    report_error: StateErrorReporter,
    state: Mutex<StateSubscriberState<T>>,
}

struct DrainRunningGuard<'a, T: Send + Sync + 'static> {
    state: &'a Mutex<StateSubscriberState<T>>,
    armed: bool,
}

impl<T: Send + Sync + 'static> Drop for DrainRunningGuard<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.state.lock().unwrap();
            state.running = false;
        }
    }
}

impl<T: Send + Sync + 'static> StateSubscriber<T> {
    fn new(listener: StateListener<T>, report_error: StateErrorReporter) -> Self {
        Self {
            inner: Arc::new(StateSubscriberInner {
                id: NEXT_STATE_LISTENER_ID.fetch_add(1, Ordering::Relaxed),
                listener,
                report_error,
                state: Mutex::new(StateSubscriberState {
                    pending: Vec::new(),
                    running: false,
                    started: false,
                    closed: false,
                }),
            }),
        }
    }

    // A cold replica can queue updates reentrantly before this subscriber's first hydration starts.
    fn push(&self, frame: StateDelivery<T>) {
        let mut state = self.inner.state.lock().unwrap();
        if state.closed {
            return;
        }
        if state.pending.len() >= 100 {
            let hydration = if state.started {
                None
            } else {
                state.pending.drain(..1).next()
            };
            state.pending.clear();
            if let Some(first) = hydration {
                state.pending.push(first);
            }
        }
        state.pending.push(frame);
    }

    fn drain(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.running || state.closed {
                return;
            }
            state.running = true;
        }

        let mut running_guard = DrainRunningGuard {
            state: &self.inner.state,
            armed: true,
        };

        loop {
            let frame = {
                let mut state = self.inner.state.lock().unwrap();
                if state.closed || state.pending.is_empty() {
                    state.running = false;
                    running_guard.armed = false;
                    return;
                }
                state.started = true;
                state.pending.remove(0)
            };

            let mut fut = (self.inner.listener)(frame.value, frame.context, frame.delivery);
            let waker = futures::task::noop_waker_ref();
            let mut cx = std::task::Context::from_waker(waker);
            match fut.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(Ok(())) => {}
                std::task::Poll::Ready(Err(error)) => self.report(error),
                std::task::Poll::Pending => {
                    // Leave `running = true` until the spawned future completes and calls `resume()`.
                    running_guard.armed = false;
                    let subscriber = self.clone();
                    let task = async move {
                        if let Err(error) = fut.await {
                            subscriber.report(error);
                        }
                        subscriber.resume();
                    };
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(task);
                    } else {
                        std::thread::spawn(move || {
                            futures::executor::block_on(task);
                        });
                    }
                    return;
                }
            }
        }
    }

    fn clear(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.pending.clear();
    }

    fn close(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.closed = true;
        state.pending.clear();
    }

    fn resume(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.running = false;
        }
        self.drain();
    }

    fn report(&self, error: pi_js::Error) {
        (self.inner.report_error)(error);
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
    source_listeners: indexmap::IndexMap<u64, ReplicatedStateFallibleOpsListener>,
    publications: Vec<Publication<T>>,
    value: T,
    sequence: i64,
    delivering: bool,
}

/// Maintains local publication order independently of how revisions are produced.
struct ReplicatedStatePublisher<T: Send + Sync + 'static> {
    inner: Arc<ReplicatedStatePublisherInner<T>>,
}

impl<T: Send + Sync + 'static> Clone for ReplicatedStatePublisher<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct ReplicatedStatePublisherInner<T: Send + Sync + 'static> {
    report_error: StateErrorReporter,
    state: Mutex<ReplicatedStatePublisherState<T>>,
}

struct DeliveringGuard<'a, T: Send + Sync + 'static> {
    state: &'a Mutex<ReplicatedStatePublisherState<T>>,
}

impl<T: Send + Sync + 'static> Drop for DeliveringGuard<'_, T> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.delivering = false;
    }
}

impl<T: Send + Sync + 'static> ReplicatedStatePublisher<T> {
    fn new(initial: T, report_error: Option<StateErrorReporter>) -> Self {
        Self {
            inner: Arc::new(ReplicatedStatePublisherInner {
                report_error: report_error.unwrap_or_else(|| Arc::new(report_error_async)),
                state: Mutex::new(ReplicatedStatePublisherState {
                    listeners: indexmap::IndexMap::new(),
                    source_listeners: indexmap::IndexMap::new(),
                    publications: Vec::new(),
                    value: initial,
                    sequence: 0,
                    delivering: false,
                }),
            }),
        }
    }

    fn value(&self) -> T
    where
        T: Clone,
    {
        self.inner.state.lock().unwrap().value.clone()
    }

    fn snapshot(&self) -> PublisherSnapshot<T>
    where
        T: Clone,
    {
        let state = self.inner.state.lock().unwrap();
        PublisherSnapshot {
            value: state.value.clone(),
            sequence: state.sequence,
        }
    }

    fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        T: Clone,
        F: Fn(T, crate::types::Context, crate::types::ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        let subscriber = StateSubscriber::new(Arc::new(listener), Arc::clone(&self.inner.report_error));
        let sub_id = subscriber.inner.id;

        // Lock ordering: `publisher.state` -> `subscriber.state`. Pushing hydration before releasing
        // `publisher.state` guarantees it enters the subscriber queue ahead of any concurrent publish.
        {
            let mut state = self.inner.state.lock().unwrap();
            let value = state.value.clone();
            let sequence = state.sequence;
            state.listeners.insert(sub_id, (subscriber.clone(), sequence));
            subscriber.push(StateDelivery {
                value,
                context: service_delivery_context(),
                delivery: crate::types::ReplicatedStateDelivery {
                    kind: crate::types::ReplicatedStateDeliveryKind::Hydrate,
                    sequence,
                },
            });
        }

        subscriber.drain();

        let publisher = self.clone();
        pi_js::Unsubscribe::new(move || {
            subscriber.close();
            let mut state = publisher.inner.state.lock().unwrap();
            state.listeners.shift_remove(&sub_id);
        })
    }

    fn subscribe_source<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(&[crate::delta::Op], i64, crate::types::Context) + Send + Sync + 'static,
    {
        self.subscribe_source_fallible(move |ops, sequence, context| {
            listener(ops, sequence, context);
            Ok(())
        })
    }

    fn subscribe_source_fallible<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(&[crate::delta::Op], i64, crate::types::Context) -> pi_js::Result<()> + Send + Sync + 'static,
    {
        let id = NEXT_STATE_LISTENER_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.inner.state.lock().unwrap();
            state.source_listeners.insert(id, Arc::new(listener));
        }
        let publisher = self.clone();
        pi_js::Unsubscribe::new(move || {
            let mut state = publisher.inner.state.lock().unwrap();
            state.source_listeners.shift_remove(&id);
        })
    }

    /// Publish an already-prepared immutable revision and return isolated listener failures.
    fn publish(&self, value: T, ops: &[crate::delta::Op], context: crate::types::Context) -> Vec<pi_js::Error>
    where
        T: Clone,
    {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.value = value.clone();
            state.sequence += 1;
            let sequence = state.sequence;
            state.publications.push(Publication {
                value,
                ops: ops.to_vec(),
                sequence,
                context,
            });
            if state.delivering {
                return Vec::new();
            }
            state.delivering = true;
        }

        let _delivering_guard = DeliveringGuard {
            state: &self.inner.state,
        };
        let mut errors: Vec<pi_js::Error> = Vec::new();

        loop {
            let (publication, source_listeners, listeners) = {
                let mut state = self.inner.state.lock().unwrap();
                if state.publications.is_empty() {
                    break;
                }
                let publication = state.publications.remove(0);
                let source_listeners: Vec<ReplicatedStateFallibleOpsListener> =
                    state.source_listeners.values().cloned().collect();
                let listeners: Vec<(StateSubscriber<T>, i64)> = state.listeners.values().cloned().collect();
                (publication, source_listeners, listeners)
            };

            for listener in source_listeners {
                if let Err(err) = listener(&publication.ops, publication.sequence, publication.context.clone()) {
                    errors.push(err);
                }
            }

            let delivery = crate::types::ReplicatedStateDelivery {
                kind: crate::types::ReplicatedStateDeliveryKind::Update,
                sequence: publication.sequence,
            };
            for (subscriber, hydrated_sequence) in listeners {
                if publication.sequence <= hydrated_sequence {
                    continue;
                }
                subscriber.push(StateDelivery {
                    value: publication.value.clone(),
                    context: publication.context.clone(),
                    delivery,
                });
                subscriber.drain();
            }
        }

        errors
    }
}

struct PublisherSource<T: Send + Sync + 'static> {
    publisher: ReplicatedStatePublisher<T>,
}

impl<T: Send + Sync + Clone + 'static> ReplicatedStateInternals for PublisherSource<T> {
    fn snapshot(&self) -> ReplicatedStateInternalsSnapshot {
        let snapshot = self.publisher.snapshot();
        let value = (&snapshot.value as &dyn std::any::Any)
            .downcast_ref::<serde_json::Value>()
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        ReplicatedStateInternalsSnapshot {
            value,
            sequence: snapshot.sequence,
        }
    }

    fn subscribe(&self, listener: ReplicatedStateOpsListener) -> pi_js::Unsubscribe {
        self.publisher
            .subscribe_source(move |ops, sequence, context| listener(ops, sequence, context))
    }

    fn subscribe_fallible(&self, listener: ReplicatedStateFallibleOpsListener) -> pi_js::Unsubscribe {
        self.publisher
            .subscribe_source_fallible(move |ops, sequence, context| listener(ops, sequence, context))
    }
}

#[derive(Clone)]
pub struct MutableReplicatedStateImpl<T: Send + Sync + 'static> {
    inner: Arc<MutableReplicatedStateImplInner<T>>,
}

struct MutableReplicatedStateImplInner<T: Send + Sync + 'static> {
    tracker: crate::delta::tracker::Tracker<T>,
    publisher: ReplicatedStatePublisher<T>,
    internals: Arc<dyn ReplicatedStateInternals>,
    changing: Mutex<bool>,
}

struct ChangingResetGuard<'a>(&'a Mutex<bool>);

impl Drop for ChangingResetGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = false;
    }
}

impl<T: Send + Sync + Clone + 'static> MutableReplicatedStateImpl<T> {
    pub fn new(initial: T) -> pi_js::Result<Self> {
        let tracker = crate::delta::tracker::track(initial);
        let initial_value = tracker.value();
        let publisher = ReplicatedStatePublisher::new(initial_value, None);
        let internals: Arc<dyn ReplicatedStateInternals> = Arc::new(PublisherSource {
            publisher: publisher.clone(),
        });
        let inner = Arc::new(MutableReplicatedStateImplInner {
            tracker,
            publisher,
            internals: Arc::clone(&internals),
            changing: Mutex::new(false),
        });
        register_replicated_state_internals(inner.clone(), internals);
        Ok(Self { inner })
    }

    pub(crate) fn internals(&self) -> Arc<dyn ReplicatedStateInternals> {
        Arc::clone(&self.inner.internals)
    }

    pub fn value(&self) -> T {
        self.inner.tracker.value()
    }

    pub fn change<F>(&self, context: crate::types::Context, mutate: F) -> pi_js::Result<()>
    where
        F: FnOnce(&mut crate::delta::draft::Draft<T>),
    {
        {
            let mut changing = self.inner.changing.lock().unwrap();
            if *changing {
                return Err(pi_js::Error::js(
                    "Error",
                    "Replicated state cannot be changed reentrantly from a change callback",
                ));
            }
            *changing = true;
        }

        let prepared = {
            let _reset_changing = ChangingResetGuard(&self.inner.changing);
            let change = self.inner.tracker.begin_change();
            let draft_result = change.state();
            let mut draft = match draft_result {
                Ok(draft) => draft,
                Err(error) => {
                    change.abort();
                    return Err(error);
                }
            };
            mutate(&mut draft);
            match change.prepare_with(draft) {
                Ok(prepared) => prepared,
                Err(error) => {
                    change.abort();
                    return Err(error);
                }
            }
        };

        self.inner.tracker.adopt(&prepared)?;
        if prepared.ops().is_empty() {
            return Ok(());
        }
        let errors = self
            .inner
            .publisher
            .publish(prepared.value().clone(), prepared.ops(), context);
        throw_collected_errors(&errors, "Replicated state listeners failed")
    }

    pub fn replace(&self, context: crate::types::Context, value: T) -> pi_js::Result<()> {
        if *self.inner.changing.lock().unwrap() {
            return Err(pi_js::Error::js(
                "Error",
                "Replicated state cannot be replaced from a change callback",
            ));
        }
        let prepared = self.inner.tracker.prepare_replace(value)?;
        self.inner.tracker.adopt(&prepared)?;
        if prepared.ops().is_empty() {
            return Ok(());
        }
        let errors = self
            .inner
            .publisher
            .publish(prepared.value().clone(), prepared.ops(), context);
        throw_collected_errors(&errors, "Replicated state listeners failed")
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(T, crate::types::Context, crate::types::ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        self.inner.publisher.subscribe(listener)
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
    internals: Arc<dyn ReplicatedStateInternals>,
    attachment: BoxedSourceAttachment<T>,
    report_error: StateErrorReporter,
    runtime: Mutex<AttachedRuntime>,
}

impl<T: Send + Sync + Clone + 'static> AttachedReplicatedStateImpl<T> {
    fn new(
        attachment: BoxedSourceAttachment<T>,
        options: Option<crate::types::ReplicatedStateSourceOptions>,
    ) -> pi_js::Result<Self> {
        let snapshot = attachment.snapshot().clone();
        assert_cursor(snapshot.cursor, CursorKind::Snapshot)?;
        let report_error = options
            .and_then(|opts| opts.on_error)
            .unwrap_or_else(|| Arc::new(report_error_async));
        let publisher = ReplicatedStatePublisher::new(snapshot.value, Some(Arc::clone(&report_error)));
        let internals: Arc<dyn ReplicatedStateInternals> = Arc::new(PublisherSource {
            publisher: publisher.clone(),
        });
        let inner = Arc::new(AttachedReplicatedStateImplInner {
            publisher,
            internals: Arc::clone(&internals),
            attachment,
            report_error,
            runtime: Mutex::new(AttachedRuntime {
                cursor: snapshot.cursor,
                disposed: false,
            }),
        });
        register_replicated_state_internals(inner.clone(), internals);
        Ok(Self { inner })
    }

    pub(crate) fn internals(&self) -> Arc<dyn ReplicatedStateInternals> {
        Arc::clone(&self.inner.internals)
    }

    pub fn value(&self) -> T {
        self.inner.publisher.value()
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(T, crate::types::Context, crate::types::ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        self.inner.publisher.subscribe(listener)
    }

    pub fn activate(&self) -> pi_js::Result<()> {
        let state = self.clone();
        self.inner.attachment.activate(Arc::new(move |frame| {
            state.receive(frame);
            Ok(())
        }))
    }

    pub fn dispose(&self) -> pi_js::Result<()> {
        {
            let mut runtime = self.inner.runtime.lock().unwrap();
            if runtime.disposed {
                return Ok(());
            }
            runtime.disposed = true;
        }
        self.inner.attachment.dispose()
    }

    fn receive(&self, frame: crate::types::ReplicatedStateSourceFrame<T>) {
        let validation = {
            let mut runtime = self.inner.runtime.lock().unwrap();
            if runtime.disposed {
                return;
            }
            if let Err(error) = assert_cursor(frame.cursor, CursorKind::Frame) {
                Err(error)
            } else {
                let expected = runtime.cursor.saturating_add(1);
                if frame.cursor != expected {
                    Err(pi_js::Error::js(
                        "Error",
                        format!(
                            "Replicated state source cursor has a gap: expected {expected}, received {}",
                            frame.cursor
                        ),
                    ))
                } else {
                    runtime.cursor = frame.cursor;
                    Ok(())
                }
            }
        };

        if let Err(error) = validation {
            self.fail(error);
            return;
        }

        let errors = self.inner.publisher.publish(frame.value, &frame.ops, frame.context);
        if errors.len() == 1 {
            self.report(clone_error(&errors[0]));
        } else if errors.len() > 1 {
            self.report(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Replicated state listeners failed".to_string(),
                    errors: errors.iter().map(clone_error).collect(),
                },
            ));
        }
    }

    fn fail(&self, error: pi_js::Error) {
        {
            let mut runtime = self.inner.runtime.lock().unwrap();
            if runtime.disposed {
                return;
            }
            runtime.disposed = true;
        }
        if let Err(dispose_error) = self.inner.attachment.dispose() {
            self.report(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Replicated state source contract failed".to_string(),
                    errors: vec![error, dispose_error],
                },
            ));
            return;
        }
        self.report(error);
    }

    fn report(&self, error: pi_js::Error) {
        (self.inner.report_error)(error);
    }
}

/// Attach a publication-only replicated state to one authoritative immutable source stream.
pub fn attach_replicated_state_source<T, S>(
    source: S,
    options: Option<crate::types::ReplicatedStateSourceOptions>,
) -> pi_js::Result<AttachedReplicatedStateImpl<T>>
where
    T: Send + Sync + Clone + 'static,
    S: crate::types::ReplicatedStateSource<T> + Send + 'static,
{
    let attachment = source.attach()?;
    match AttachedReplicatedStateImpl::new(attachment, options) {
        Ok(state) => {
            if let Err(error) = state.activate() {
                if let Err(dispose_error) = state.dispose() {
                    return Err(pi_js::Error::typed(
                        "AggregateError",
                        AggregateError {
                            message: "Failed to attach replicated state source".to_string(),
                            errors: vec![error, dispose_error],
                        },
                    ));
                }
                return Err(error);
            }
            Ok(state)
        }
        Err(error) => Err(error),
    }
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

impl<T: Send + Sync + Clone + 'static> ReplicatedStateReplica<T> {
    pub fn new(report_error: impl Fn(pi_js::Error) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(ReplicatedStateReplicaInner {
                report_error: Arc::new(report_error),
                validator: crate::delta::revision_validator::JsonRevisionValidator::new(),
                state: Mutex::new(ReplicaState {
                    listeners: indexmap::IndexMap::new(),
                    value: None,
                    sequence: None,
                }),
            }),
        }
    }

    pub fn value(&self) -> Option<T> {
        self.inner.state.lock().unwrap().value.clone()
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(T, crate::types::Context, crate::types::ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        let subscriber = StateSubscriber::new(Arc::new(listener), Arc::clone(&self.inner.report_error));
        let sub_id = subscriber.inner.id;

        {
            let mut state = self.inner.state.lock().unwrap();
            state.listeners.insert(sub_id, subscriber.clone());
            if let (Some(value), Some(sequence)) = (state.value.clone(), state.sequence) {
                subscriber.push(StateDelivery {
                    value,
                    context: service_delivery_context(),
                    delivery: crate::types::ReplicatedStateDelivery {
                        kind: crate::types::ReplicatedStateDeliveryKind::Hydrate,
                        sequence,
                    },
                });
            }
        }

        subscriber.drain();

        let replica = self.clone();
        pi_js::Unsubscribe::new(move || {
            subscriber.close();
            let mut state = replica.inner.state.lock().unwrap();
            state.listeners.shift_remove(&sub_id);
        })
    }

    pub fn hydrate(
        &self,
        sequence: i64,
        ops: &[crate::delta::Op],
        context: crate::types::Context,
    ) -> pi_js::Result<()> {
        if !crate::delta::is_base(ops) {
            self.clear();
            return Err(pi_js::Error::js(
                "Error",
                "Replicated state snapshot is not a base operation batch",
            ));
        }
        let applied: T = match crate::delta::apply_immutable(None, ops) {
            Ok(next) => next,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };
        let next = match self.inner.validator.validate(applied) {
            Ok(valid) => valid,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            state.sequence = Some(sequence);
            state.value = Some(next);
        }

        self.deliver_all(
            context,
            crate::types::ReplicatedStateDelivery {
                kind: crate::types::ReplicatedStateDeliveryKind::Hydrate,
                sequence,
            },
        );
        Ok(())
    }

    pub fn update(&self, sequence: i64, ops: &[crate::delta::Op], context: crate::types::Context) -> pi_js::Result<()> {
        let prev = {
            let state = self.inner.state.lock().unwrap();
            let (Some(current_seq), Some(current_val)) = (state.sequence, state.value.as_ref()) else {
                return Err(pi_js::Error::js(
                    "Error",
                    "Replicated state received an update before hydration",
                ));
            };
            if sequence != current_seq + 1 {
                drop(state);
                self.clear();
                return Err(pi_js::Error::js("Error", "Replicated state update sequence has a gap"));
            }
            current_val.clone()
        };

        let applied: T = match crate::delta::apply_immutable(Some(prev), ops) {
            Ok(next) => next,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };
        let next = match self.inner.validator.validate(applied) {
            Ok(valid) => valid,
            Err(error) => {
                self.clear();
                return Err(error);
            }
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            state.sequence = Some(sequence);
            state.value = Some(next);
        }

        self.deliver_all(
            context,
            crate::types::ReplicatedStateDelivery {
                kind: crate::types::ReplicatedStateDeliveryKind::Update,
                sequence,
            },
        );
        Ok(())
    }

    pub fn clear(&self) {
        let subscribers: Vec<StateSubscriber<T>> = {
            let mut state = self.inner.state.lock().unwrap();
            state.value = None;
            state.sequence = None;
            state.listeners.values().cloned().collect()
        };
        for subscriber in subscribers {
            subscriber.clear();
        }
    }

    // Enqueue for everyone before user code can publish another revision reentrantly.
    fn deliver_all(&self, context: crate::types::Context, delivery: crate::types::ReplicatedStateDelivery) {
        let (value, subscribers) = {
            let state = self.inner.state.lock().unwrap();
            let Some(value) = state.value.clone() else {
                return;
            };
            let subscribers: Vec<StateSubscriber<T>> = state.listeners.values().cloned().collect();
            (value, subscribers)
        };

        for subscriber in &subscribers {
            subscriber.push(StateDelivery {
                value: value.clone(),
                context: context.clone(),
                delivery,
            });
        }
        for subscriber in subscribers {
            subscriber.drain();
        }
    }
}

/// @internal Context for synthetic service deliveries without a caller.
// TODO: Add delivery-scoped cancellation or metadata if deliveries gain an owned lifecycle.
pub fn service_delivery_context() -> crate::types::Context {
    crate::context::BACKGROUND_CONTEXT.clone()
}

enum CursorKind {
    Snapshot,
    Frame,
}

fn assert_cursor(cursor: i64, kind: CursorKind) -> pi_js::Result<()> {
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&cursor) {
        let kind_name = match kind {
            CursorKind::Snapshot => "snapshot",
            CursorKind::Frame => "frame",
        };
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Replicated state source {kind_name} cursor must be a safe integer"),
        ));
    }
    Ok(())
}

fn throw_collected_errors(errors: &[pi_js::Error], message: &str) -> pi_js::Result<()> {
    crate::services::errors::throw_collected_errors(errors, message)
}

fn report_error_async(error: pi_js::Error) {
    pi_js::console::error(&error.to_js_string());
}

//! Port of packages/chord/src/services/consumer.ts

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use indexmap::{IndexMap, IndexSet};

use crate::context::{BACKGROUND_CONTEXT, await_with_context};
use crate::delta::Op;
use crate::services::errors::{AggregateError, RemoteServiceError, RemoteServiceErrorCode, clone_error};
use crate::services::handle::{PropertyKey, ServiceObject, ServiceSlot, ServiceTarget, ServiceValue};
use crate::services::instances::{InstanceDirectory, InstanceDirectoryOptions, InstanceEntry};
use crate::services::state::{ReplicatedStateReplica, service_delivery_context};
use crate::types::{
    Context, JsonValue, RemoteServiceBindingOptions, RemoteServiceTransport, ReplicatedStateDelivery, Service,
    ServiceInstanceAddress, ServiceInstanceSnapshot, ServiceMemberSnapshot, ServiceMode, ServiceProviderUpdate,
    ServiceSubscription, ServiceSubscriptionSnapshot,
};

type ErrorReporter = Arc<dyn Fn(pi_js::Error) + Send + Sync>;
type AssertAccess = Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>;
type IsActive = Arc<dyn Fn() -> bool + Send + Sync>;
type InvokeFn =
    Arc<dyn Fn(Vec<JsonValue>, Context) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> + Send + Sync>;
type ObserveHandler = Arc<dyn Fn(ServiceObject, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;
type StateListener =
    Arc<dyn Fn(JsonValue, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ServiceMemberKind {
    Method,
    State,
}

/// Optional wrapper allowing typed Rust state listeners to be passed directly through `ServiceObject::apply`.
pub(crate) struct TypedStateListener(pub StateListener);

impl ServiceTarget for TypedStateListener {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js(
            "TypeError",
            "TypedStateListener cannot be applied directly",
        ))
    }

    fn is_callable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

struct UnsubscribeTarget(Mutex<Option<pi_js::Unsubscribe>>);

impl ServiceTarget for UnsubscribeTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        if let Some(unsub) = self.0.lock().unwrap().take() {
            unsub.call();
        }
        Ok(ServiceValue::Undefined)
    }

    fn is_callable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(self)
    }
}

// PORT: a JS promise settles once, runs eagerly on creation, and can be awaited any number of times.
#[derive(Clone)]
struct SharedPromise {
    inner: Arc<SharedPromiseInner>,
}

struct SharedPromiseInner {
    rx: tokio::sync::watch::Receiver<Option<std::result::Result<(), Arc<pi_js::Error>>>>,
}

impl SharedPromise {
    fn resolved(result: pi_js::Result<()>) -> Self {
        let (_tx, rx) = tokio::sync::watch::channel(Some(result.map_err(Arc::new)));
        Self {
            inner: Arc::new(SharedPromiseInner { rx }),
        }
    }

    fn spawn(mut future: pi_js::BoxFuture<pi_js::Result<()>>) -> Self {
        let waker = futures::task::noop_waker_ref();
        let mut cx = std::task::Context::from_waker(waker);
        if let std::task::Poll::Ready(res) = future.as_mut().poll(&mut cx) {
            return Self::resolved(res);
        }
        let (tx, rx) = tokio::sync::watch::channel(None);
        let task = async move {
            let res = future.await.map_err(Arc::new);
            let _ = tx.send(Some(res));
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(task);
        } else {
            std::thread::spawn(move || {
                futures::executor::block_on(task);
            });
        }
        Self {
            inner: Arc::new(SharedPromiseInner { rx }),
        }
    }

    async fn wait(&self) -> pi_js::Result<()> {
        let mut rx = self.inner.rx.clone();
        loop {
            if let Some(res) = rx.borrow_and_update().as_ref() {
                return match res {
                    Ok(()) => Ok(()),
                    Err(err) => Err(clone_error(err)),
                };
            }
            if rx.changed().await.is_err() {
                if let Some(res) = rx.borrow().as_ref() {
                    return match res {
                        Ok(()) => Ok(()),
                        Err(err) => Err(clone_error(err)),
                    };
                }
                return Ok(());
            }
        }
    }
}

#[derive(Clone)]
struct MemberSlot {
    inner: Arc<MemberSlotInner>,
}

struct MemberSlotKinds {
    kind: Option<ServiceMemberKind>,
    expected_kind: Option<ServiceMemberKind>,
}

struct MemberSlotInner {
    weak_self: Weak<Self>,
    service_id: String,
    member: String,
    invoke: InvokeFn,
    state: ReplicatedStateReplica,
    is_active: IsActive,
    assert_access: AssertAccess,
    kinds: Mutex<MemberSlotKinds>,
}

impl MemberSlot {
    fn new(
        service_id: &str,
        member: &str,
        invoke: InvokeFn,
        is_active: IsActive,
        assert_access: AssertAccess,
        report_error: ErrorReporter,
    ) -> Self {
        Self {
            inner: Arc::new_cyclic(|weak| MemberSlotInner {
                weak_self: weak.clone(),
                service_id: service_id.to_string(),
                member: member.to_string(),
                invoke,
                state: ReplicatedStateReplica::new(move |err| report_error(err)),
                is_active,
                assert_access,
                kinds: Mutex::new(MemberSlotKinds {
                    kind: None,
                    expected_kind: None,
                }),
            }),
        }
    }

    fn value(&self) -> ServiceObject {
        ServiceObject::new(Arc::clone(&self.inner) as Arc<dyn ServiceTarget>)
    }

    fn set_description(&self, kind: ServiceMemberKind) -> pi_js::Result<()> {
        let mut kinds = self.inner.kinds.lock().unwrap();
        if let Some(current) = kinds.kind {
            if current != kind {
                return Err(pi_js::Error::js(
                    "Error",
                    format!(
                        "Remote service member {}.{} changed kind",
                        self.inner.service_id, self.inner.member
                    ),
                ));
            }
        }
        kinds.kind = Some(kind);
        if let Some(expected) = kinds.expected_kind {
            if expected != kind {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceMemberMismatch,
                    format!(
                        "Remote service member {}.{} is {}, not {}",
                        self.inner.service_id,
                        self.inner.member,
                        kind_str(kind),
                        kind_str(expected),
                    ),
                )
                .into_js_error());
            }
        }
        Ok(())
    }

    fn hydrate(&self, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        self.set_description(ServiceMemberKind::State)?;
        self.inner.state.hydrate(sequence, ops, context)
    }

    fn update(&self, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        self.set_description(ServiceMemberKind::State)?;
        self.inner.state.update(sequence, ops, context)
    }

    fn clear(&self) {
        self.inner.state.clear();
    }
}

impl MemberSlotInner {
    fn expect(&self, kind: ServiceMemberKind) -> pi_js::Result<()> {
        let mut kinds = self.kinds.lock().unwrap();
        if let Some(expected) = kinds.expected_kind {
            if expected != kind {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceMemberMismatch,
                    format!(
                        "Remote service member {}.{} was used as two different kinds",
                        self.service_id, self.member
                    ),
                )
                .into_js_error());
            }
        }
        kinds.expected_kind = Some(kind);
        if let Some(actual) = kinds.kind {
            if actual != kind {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceMemberMismatch,
                    format!(
                        "Remote service member {}.{} is {}, not {}",
                        self.service_id,
                        self.member,
                        kind_str(actual),
                        kind_str(kind),
                    ),
                )
                .into_js_error());
            }
        }
        Ok(())
    }

    fn subscribe_from_args(self: &Arc<Self>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        (self.assert_access)()?;
        let listener = match args.first() {
            Some(ServiceValue::Object(obj)) => {
                if let Some(typed) = obj.downcast_ref::<TypedStateListener>() {
                    Arc::clone(&typed.0)
                } else if obj.is_callable() {
                    let cb = obj.clone();
                    Arc::new(move |val: JsonValue, ctx: Context, delivery: ReplicatedStateDelivery| {
                        let callback = cb.clone();
                        Box::pin(async move {
                            let delivery_json = serde_json::to_value(delivery)
                                .map_err(|err| pi_js::Error::js("Error", err.to_string()))?;
                            let cb_args = vec![
                                ServiceValue::from_json(&val),
                                ServiceValue::Object(ServiceObject::from_context(ctx)),
                                ServiceValue::from_json(&delivery_json),
                            ];
                            let outcome = callback.apply(None, &cb_args)?;
                            if let ServiceValue::Object(ret_obj) = outcome {
                                if let Some(fut) = ret_obj.into_json_future() {
                                    let _ = fut.await?;
                                }
                            }
                            Ok(())
                        }) as pi_js::BoxFuture<pi_js::Result<()>>
                    })
                } else {
                    return Err(pi_js::Error::js(
                        "TypeError",
                        "Replicated state subscription listener must be a function",
                    ));
                }
            }
            _ => {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Replicated state subscription listener must be a function",
                ));
            }
        };
        self.expect(ServiceMemberKind::State)?;
        let unsubscribe = self
            .state
            .subscribe(move |val, ctx, delivery| listener(val, ctx, delivery));
        Ok(ServiceValue::Object(ServiceObject::new(Arc::new(UnsubscribeTarget(
            Mutex::new(Some(unsubscribe)),
        )))))
    }

    // PORT: `#call` validates access/kind synchronously and rejects remaining contract errors as a Promise.
    fn call(&self, args: &[ServiceValue]) -> pi_js::Result<pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>>> {
        (self.assert_access)()?;
        self.expect(ServiceMemberKind::Method)?;

        if !(self.is_active)() {
            let service_id = self.service_id.clone();
            return Ok(Box::pin(async move {
                Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceStaleInstance,
                    format!("Remote service {service_id} binding is closed"),
                )
                .into_js_error())
            }));
        }

        let context = args.last().and_then(|last| match last {
            ServiceValue::Object(obj) => obj.as_context(),
            _ => None,
        });
        let Some(context) = context else {
            let service_id = self.service_id.clone();
            let member = self.member.clone();
            return Ok(Box::pin(async move {
                Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceInvalidValue,
                    format!("Remote service method {service_id}.{member} requires a trailing Context"),
                )
                .into_js_error())
            }));
        };

        let mut business_args = Vec::with_capacity(args.len().saturating_sub(1));
        for arg in &args[..args.len() - 1] {
            match arg.to_json() {
                Ok(json) => business_args.push(json),
                Err(err) => {
                    return Ok(Box::pin(async move { Err(err) }));
                }
            }
        }

        let invoke = Arc::clone(&self.invoke);
        Ok(Box::pin(async move { invoke(business_args, context).await }))
    }
}

impl ServiceTarget for MemberSlotInner {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        match property {
            PropertyKey::String(name) if name == "value" => {
                (self.assert_access)()?;
                self.expect(ServiceMemberKind::State)?;
                Ok(self
                    .state
                    .value()
                    .map(|value| ServiceValue::from_json(&value))
                    .unwrap_or(ServiceValue::Undefined))
            }
            PropertyKey::String(name) if name == "subscribe" => {
                let slot = self.weak_self.upgrade().ok_or_else(|| {
                    pi_js::Error::js(
                        "Error",
                        format!("Remote service {}.{} is no longer active", self.service_id, self.member),
                    )
                })?;
                Ok(ServiceValue::Object(ServiceObject::from_fn(move |_this, args| {
                    slot.subscribe_from_args(args)
                })))
            }
            PropertyKey::Symbol(sym) if sym == "toStringTag" => {
                Ok(ServiceValue::String("RemoteServiceMember".to_string()))
            }
            _ => Ok(ServiceValue::Undefined),
        }
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        let future = self.call(args)?;
        Ok(ServiceValue::Object(ServiceObject::from_promise(future)))
    }

    fn is_callable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(&self.state)
    }
}

#[derive(Clone)]
struct ServiceFacade {
    inner: Arc<ServiceFacadeInner>,
}

struct FacadeSlotsState {
    slots: IndexMap<String, MemberSlot>,
    descriptions: IndexMap<String, ServiceMemberKind>,
}

struct ServiceFacadeInner {
    weak_self: Weak<Self>,
    service_id: String,
    address: Option<ServiceInstanceAddress>,
    transport: RemoteServiceTransport,
    report_error: ErrorReporter,
    state: Mutex<FacadeSlotsState>,
    is_active: IsActive,
    assert_access: AssertAccess,
}

impl ServiceFacade {
    fn new(
        service_id: &str,
        address: Option<ServiceInstanceAddress>,
        transport: RemoteServiceTransport,
        is_active: IsActive,
        assert_access: AssertAccess,
        report_error: ErrorReporter,
    ) -> Self {
        Self {
            inner: Arc::new_cyclic(|weak| ServiceFacadeInner {
                weak_self: weak.clone(),
                service_id: service_id.to_string(),
                address,
                transport,
                report_error,
                state: Mutex::new(FacadeSlotsState {
                    slots: IndexMap::new(),
                    descriptions: IndexMap::new(),
                }),
                is_active,
                assert_access,
            }),
        }
    }

    fn proxy(&self) -> ServiceObject {
        ServiceObject::new(Arc::clone(&self.inner) as Arc<dyn ServiceTarget>)
    }

    fn install(&self, snapshot: ServiceInstanceSnapshot, context: Context) -> pi_js::Result<()> {
        if !same_address(snapshot.instance.as_ref(), self.inner.address.as_ref()) {
            return Err(pi_js::Error::js(
                "Error",
                "Remote service snapshot has the wrong address",
            ));
        }
        let members = validate_members(&snapshot.members)?;

        enum SlotAction {
            Hydrate {
                slot: MemberSlot,
                sequence: i64,
                ops: Vec<Op>,
            },
            SetMethod {
                slot: MemberSlot,
            },
        }

        let actions = {
            let mut state = self.inner.state.lock().unwrap();
            for name in state.slots.keys() {
                if !members.contains_key(name) {
                    return Err(RemoteServiceError::new(
                        RemoteServiceErrorCode::ServiceMemberNotFound,
                        format!("Unknown remote service member {}.{}", self.inner.service_id, name),
                    )
                    .into_js_error());
                }
            }
            state.descriptions.clear();
            for (name, member) in &members {
                let kind = match member {
                    ServiceMemberSnapshot::Method(_) => ServiceMemberKind::Method,
                    ServiceMemberSnapshot::State(_) => ServiceMemberKind::State,
                };
                state.descriptions.insert(name.clone(), kind);
            }
            let mut actions = Vec::new();
            for member in members.values() {
                match member {
                    ServiceMemberSnapshot::State(state_snap) => {
                        let slot = get_or_create_slot_locked(&self.inner, &mut state, &state_snap.name);
                        actions.push(SlotAction::Hydrate {
                            slot,
                            sequence: state_snap.sequence,
                            ops: state_snap.ops.clone(),
                        });
                    }
                    ServiceMemberSnapshot::Method(method_snap) => {
                        if let Some(slot) = state.slots.get(&method_snap.name).cloned() {
                            actions.push(SlotAction::SetMethod { slot });
                        }
                    }
                }
            }
            actions
        };

        for action in actions {
            match action {
                SlotAction::Hydrate { slot, sequence, ops } => {
                    slot.hydrate(sequence, &ops, context.clone())?;
                }
                SlotAction::SetMethod { slot } => {
                    slot.set_description(ServiceMemberKind::Method)?;
                }
            }
        }
        Ok(())
    }

    fn update(&self, member: &str, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        let slot = {
            let mut state = self.inner.state.lock().unwrap();
            if state.descriptions.get(member) != Some(&ServiceMemberKind::State) {
                return Err(pi_js::Error::js(
                    "Error",
                    format!(
                        "Remote service update targets non-state member {}.{}",
                        self.inner.service_id, member
                    ),
                ));
            }
            get_or_create_slot_locked(&self.inner, &mut state, member)
        };
        slot.update(sequence, ops, context)
    }

    fn clear(&self) {
        let slots: Vec<MemberSlot> = {
            let state = self.inner.state.lock().unwrap();
            state.slots.values().cloned().collect()
        };
        for slot in slots {
            slot.clear();
        }
    }
}

fn get_or_create_slot_locked(
    facade: &Arc<ServiceFacadeInner>,
    state: &mut FacadeSlotsState,
    member: &str,
) -> MemberSlot {
    if let Some(slot) = state.slots.get(member) {
        return slot.clone();
    }
    let transport = facade.transport.clone();
    let service_id = facade.service_id.clone();
    let address = facade.address.clone();
    let member_name = member.to_string();
    let invoke: InvokeFn = Arc::new(move |args, context| {
        let transport = transport.clone();
        let call = crate::types::ServiceCall {
            service_id: service_id.clone(),
            instance: address.clone(),
            member: member_name.clone(),
            args,
        };
        Box::pin(async move { transport.invoke(&call, &context).await })
    });
    let slot = MemberSlot::new(
        &facade.service_id,
        member,
        invoke,
        Arc::clone(&facade.is_active),
        Arc::clone(&facade.assert_access),
        Arc::clone(&facade.report_error),
    );
    if let Some(kind) = state.descriptions.get(member).copied() {
        let _ = slot.set_description(kind);
    }
    state.slots.insert(member.to_string(), slot.clone());
    slot
}

impl ServiceTarget for ServiceFacadeInner {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let PropertyKey::String(name) = property else {
            return Ok(ServiceValue::Undefined);
        };
        let Some(facade) = self.weak_self.upgrade() else {
            return Ok(ServiceValue::Undefined);
        };
        let mut state = facade.state.lock().unwrap();
        let slot = get_or_create_slot_locked(&facade, &mut state, name);
        Ok(ServiceValue::Object(slot.value()))
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "Remote service proxy is not callable"))
    }
}

struct SingletonBinding {
    facade: ServiceFacade,
    subscription: Option<ServiceSubscription>,
    starting: Option<SharedPromise>,
    active: Arc<AtomicBool>,
    revision: i64,
}

#[derive(Clone)]
struct KeyedInstance {
    key: String,
    generation: i64,
    service: ServiceObject,
    deactivate: Arc<dyn Fn() + Send + Sync>,
    facade: ServiceFacade,
}

impl InstanceEntry for KeyedInstance {
    fn key(&self) -> &str {
        &self.key
    }

    fn generation(&self) -> i64 {
        self.generation
    }

    fn service(&self) -> ServiceObject {
        self.service.clone()
    }

    fn deactivate(&self) {
        (self.deactivate)()
    }
}

struct KeyedBindingState {
    subscription: Option<ServiceSubscription>,
    starting: Option<SharedPromise>,
    closed: bool,
    bound: bool,
    revision: i64,
}

#[derive(Clone)]
struct KeyedBinding {
    inner: Arc<KeyedBindingInner>,
}

struct KeyedBindingInner {
    service: Service,
    transport: RemoteServiceTransport,
    report_error: ErrorReporter,
    assert_access: AssertAccess,
    on_empty: Arc<dyn Fn() + Send + Sync>,
    instances: InstanceDirectory<KeyedInstance>,
    state: Mutex<KeyedBindingState>,
}

impl KeyedBinding {
    fn new(
        service: Service,
        transport: RemoteServiceTransport,
        report_error: ErrorReporter,
        assert_access: AssertAccess,
        on_empty: Arc<dyn Fn() + Send + Sync>,
        bound: bool,
    ) -> Self {
        let reporter = Arc::clone(&report_error);
        Self {
            inner: Arc::new(KeyedBindingInner {
                service,
                transport,
                report_error,
                assert_access,
                on_empty,
                instances: InstanceDirectory::new(InstanceDirectoryOptions {
                    ready: false,
                    on_error: reporter,
                }),
                state: Mutex::new(KeyedBindingState {
                    subscription: None,
                    starting: None,
                    closed: false,
                    bound,
                    revision: 0,
                }),
            }),
        }
    }

    fn observe(&self, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        {
            let state = self.inner.state.lock().unwrap();
            if state.closed {
                return Err(pi_js::Error::js("Error", "Remote keyed service binding is closed"));
            }
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let service_id = self.inner.service.id.clone();
        let assert_access = Arc::clone(&self.inner.assert_access);
        let stopped_for_view = Arc::clone(&stopped);
        let stop = self.inner.instances.observe(Arc::new(move |service, context| {
            let slot = ServiceSlot::new(&service_id, true);
            slot.bind(service);
            let view_assert_access = Arc::clone(&assert_access);
            let view_stopped = Arc::clone(&stopped_for_view);
            let view_ctx = context.clone();
            let view_service_id = service_id.clone();
            let view = slot.view(Arc::new(move || {
                view_assert_access()?;
                if view_stopped.load(Ordering::Acquire) || view_ctx.abort_signal().is_some_and(|sig| sig.aborted()) {
                    return Err(RemoteServiceError::new(
                        RemoteServiceErrorCode::ServiceStaleInstance,
                        format!("Remote service {view_service_id} observation is closed"),
                    )
                    .into_js_error());
                }
                Ok(())
            }));
            handler(view, context)
        }))?;

        let start_info = {
            let state = self.inner.state.lock().unwrap();
            if !state.closed && state.bound && state.starting.is_none() {
                Some(state.revision)
            } else {
                None
            }
        };

        if let Some(revision) = start_info {
            let binding = self.clone();
            let starting = SharedPromise::spawn(Box::pin(async move {
                let res = binding.start(revision).await;
                if let Err(ref error) = res {
                    let should_report = {
                        let state = binding.inner.state.lock().unwrap();
                        !state.closed && state.revision == revision && state.bound
                    };
                    if should_report {
                        (binding.inner.report_error)(clone_error(error));
                    }
                }
                res
            }));
            {
                let mut state = self.inner.state.lock().unwrap();
                if !state.closed && state.bound && state.revision == revision && state.starting.is_none() {
                    state.starting = Some(starting);
                }
            }
        }

        let binding = self.clone();
        let stopped_for_unsub = stopped;
        Ok(pi_js::Unsubscribe::new(move || {
            if stopped_for_unsub.swap(true, Ordering::AcqRel) {
                return;
            }
            stop.call();
            if binding.inner.instances.observer_count() == 0 {
                (binding.inner.on_empty)();
            }
        }))
    }

    async fn rebind(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        let revision = {
            let mut state = self.inner.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.bound = bound;
            state.revision += 1;
            state.revision
        };

        self.reset(context, false).await?;

        let should_start = {
            let state = self.inner.state.lock().unwrap();
            !state.closed
                && state.revision == revision
                && state.bound == bound
                && bound
                && self.inner.instances.observer_count() > 0
        };
        if !should_start {
            return Ok(());
        }

        let binding = self.clone();
        let starting = SharedPromise::spawn(Box::pin(async move { binding.start(revision).await }));
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.closed && state.revision == revision {
                state.starting = Some(starting.clone());
            }
        }
        starting.wait().await
    }

    // PORT: `ready` synchronously captures the pending startup promise.
    fn ready(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        let starting = self.inner.state.lock().unwrap().starting.clone();
        match starting {
            Some(promise) => Box::pin(async move { promise.wait().await }),
            None => Box::pin(futures::future::ready(Ok(()))),
        }
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.revision += 1;
        }
        let reset_res = self.reset(context, true).await;
        self.inner.instances.dispose();
        reset_res
    }

    async fn reset(&self, context: Context, wait_for_starting: bool) -> pi_js::Result<()> {
        self.inner.instances.reset();
        let (starting, subscription) = {
            let mut state = self.inner.state.lock().unwrap();
            (state.starting.take(), state.subscription.take())
        };
        let wait_start = async move {
            if wait_for_starting {
                if let Some(starting) = starting {
                    let _ = starting.wait().await;
                }
            }
        };
        let close_sub = async move {
            match subscription {
                Some(sub) => sub.close(Some(context)).await,
                None => Ok(()),
            }
        };
        let (_, close_res) = tokio::join!(wait_start, close_sub);
        close_res
    }

    async fn start(&self, revision: i64) -> pi_js::Result<()> {
        let binding_weak = Arc::downgrade(&self.inner);
        let listener = Arc::new(move |update: ServiceProviderUpdate, context: Context| {
            let Some(inner) = binding_weak.upgrade() else {
                return;
            };
            let current_rev = inner.state.lock().unwrap().revision;
            if current_rev == revision {
                KeyedBinding { inner }.update(update, context);
            }
        });

        let subscription = self
            .inner
            .transport
            .subscribe(
                &self.inner.service.id,
                ServiceMode::Keyed,
                listener,
                &BACKGROUND_CONTEXT,
            )
            .await?;
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::yield_now().await;
        }

        let active = {
            let mut state = self.inner.state.lock().unwrap();
            if state.closed || !state.bound || state.revision != revision {
                false
            } else {
                state.subscription = Some(subscription.clone());
                true
            }
        };
        if !active {
            let _ = subscription.close(Some(BACKGROUND_CONTEXT.clone())).await;
            return Ok(());
        }

        let snapshot = subscription.snapshot();
        if snapshot.mode != ServiceMode::Keyed || snapshot.service_id != self.inner.service.id {
            return Err(pi_js::Error::js(
                "Error",
                format!(
                    "Remote service {} returned the wrong keyed snapshot",
                    self.inner.service.id
                ),
            ));
        }
        for inst_snapshot in &snapshot.instances {
            self.spawn(inst_snapshot.clone(), service_delivery_context())?;
        }
        subscription.activate()?;
        self.inner.instances.ready()?;
        Ok(())
    }

    fn update(&self, update: ServiceProviderUpdate, context: Context) {
        if self.inner.state.lock().unwrap().closed {
            return;
        }
        let outcome: pi_js::Result<()> = (|| match update {
            ServiceProviderUpdate::Reset { snapshot } => {
                validate_reset_snapshot(&snapshot, &self.inner.service.id, ServiceMode::Keyed)?;
                let mut snapshots = IndexMap::new();
                for inst in snapshot.instances {
                    if let Some(addr) = inst.instance.as_ref() {
                        snapshots.insert(addr.key.clone(), inst);
                    }
                }
                for current in self.inner.instances.values() {
                    let same_gen = snapshots
                        .get(&current.key)
                        .and_then(|s| s.instance.as_ref())
                        .is_some_and(|a| a.generation == current.generation);
                    if !same_gen {
                        self.inner.instances.remove(&current);
                    }
                }
                for (key, inst_snapshot) in snapshots {
                    if let Some(existing) = self.inner.instances.get(&key) {
                        existing.facade.install(inst_snapshot, context.clone())?;
                    } else {
                        self.spawn(inst_snapshot, context.clone())?;
                    }
                }
                Ok(())
            }
            ServiceProviderUpdate::Unavailable | ServiceProviderUpdate::Replaced { .. } => Err(pi_js::Error::js(
                "Error",
                "Keyed service received a singleton lifecycle update",
            )),
            ServiceProviderUpdate::Spawned { instance } => self.spawn(instance, context),
            ServiceProviderUpdate::Closed { instance } => {
                if let Some(current) = self.inner.instances.get(&instance.key) {
                    if current.generation == instance.generation {
                        self.inner.instances.remove(&current);
                    }
                }
                Ok(())
            }
            ServiceProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let Some(address) = instance else {
                    return Err(pi_js::Error::js("Error", "Keyed state update has no instance address"));
                };
                let Some(current) = self.inner.instances.get(&address.key) else {
                    return Ok(());
                };
                if current.generation != address.generation {
                    return Ok(());
                }
                current.facade.update(&member, sequence, &ops, context)
            }
        })();

        if let Err(error) = outcome {
            (self.inner.report_error)(error);
        }
    }

    fn spawn(&self, snapshot: ServiceInstanceSnapshot, context: Context) -> pi_js::Result<()> {
        let Some(address) = snapshot.instance.clone() else {
            return Err(pi_js::Error::js(
                "Error",
                "Keyed service instance snapshot has no address",
            ));
        };
        let active = Arc::new(AtomicBool::new(true));
        let active_for_facade = Arc::clone(&active);
        let weak_inner = Arc::downgrade(&self.inner);
        let is_active: IsActive = Arc::new(move || {
            let Some(inner) = weak_inner.upgrade() else {
                return false;
            };
            active_for_facade.load(Ordering::Acquire) && !inner.state.lock().unwrap().closed
        });
        let facade = ServiceFacade::new(
            &self.inner.service.id,
            Some(address.clone()),
            self.inner.transport.clone(),
            is_active,
            Arc::clone(&self.inner.assert_access),
            Arc::clone(&self.inner.report_error),
        );
        facade.install(snapshot, context)?;
        let service = facade.proxy();
        let facade_for_deactivate = facade.clone();
        let active_for_deactivate = active;
        self.inner.instances.replace(KeyedInstance {
            key: address.key,
            generation: address.generation,
            service,
            facade,
            deactivate: Arc::new(move || {
                active_for_deactivate.store(false, Ordering::Release);
                facade_for_deactivate.clear();
            }),
        })
    }
}

struct BindingState {
    modes: IndexMap<String, ServiceMode>,
    singletons: IndexMap<String, SingletonBinding>,
    keyed: IndexMap<String, KeyedBinding>,
    bound: bool,
    readiness_revision: i64,
    disposed: bool,
}

#[derive(Clone)]
pub struct RemoteServiceBindingImpl {
    inner: Arc<RemoteServiceBindingInner>,
}

struct RemoteServiceBindingInner {
    transport: RemoteServiceTransport,
    allowlist: IndexSet<String>,
    report_error: ErrorReporter,
    assert_access: AssertAccess,
    bound_flag: AtomicBool,
    disposed_flag: AtomicBool,
    state: Mutex<BindingState>,
    binding_transition: Mutex<SharedPromise>,
}

impl RemoteServiceBindingImpl {
    pub fn new(options: RemoteServiceBindingOptions) -> pi_js::Result<Self> {
        let mut allowlist = IndexSet::new();
        for svc in &options.services {
            if !allowlist.insert(svc.id.clone()) {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Remote service binding has duplicate service IDs",
                ));
            }
        }
        let bound = options.bound.unwrap_or(true);
        let report_error = options.on_error.unwrap_or_else(|| Arc::new(|_| {}));
        let assert_access = options.assert_access.unwrap_or_else(|| Arc::new(|| Ok(())));
        Ok(Self {
            inner: Arc::new(RemoteServiceBindingInner {
                transport: options.transport,
                allowlist,
                report_error,
                assert_access,
                bound_flag: AtomicBool::new(bound),
                disposed_flag: AtomicBool::new(false),
                state: Mutex::new(BindingState {
                    modes: IndexMap::new(),
                    singletons: IndexMap::new(),
                    keyed: IndexMap::new(),
                    bound,
                    readiness_revision: 0,
                    disposed: false,
                }),
                binding_transition: Mutex::new(SharedPromise::resolved(Ok(()))),
            }),
        })
    }

    pub fn r#use(&self, service: &Service) -> pi_js::Result<ServiceObject> {
        self.assert_remotable(service)?;
        let (proxy, revision_to_start, singleton_active) = {
            let mut state = self.inner.state.lock().unwrap();
            self.assert_available_locked(&mut state, &service.id, ServiceMode::Singleton)?;
            if let Some(binding) = state.singletons.get(&service.id) {
                return Ok(binding.facade.proxy());
            }

            let active = Arc::new(AtomicBool::new(true));
            let active_for_facade = Arc::clone(&active);
            let weak_inner = Arc::downgrade(&self.inner);
            let is_active: IsActive = Arc::new(move || {
                let Some(inner) = weak_inner.upgrade() else {
                    return false;
                };
                active_for_facade.load(Ordering::Acquire)
                    && !inner.disposed_flag.load(Ordering::Acquire)
                    && inner.bound_flag.load(Ordering::Acquire)
            });

            let weak_for_access = Arc::downgrade(&self.inner);
            let assert_access: AssertAccess = Arc::new(move || {
                let Some(inner) = weak_for_access.upgrade() else {
                    return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
                };
                if inner.disposed_flag.load(Ordering::Acquire) {
                    return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
                }
                (inner.assert_access)()
            });

            let facade = ServiceFacade::new(
                &service.id,
                None,
                self.inner.transport.clone(),
                is_active,
                assert_access,
                Arc::clone(&self.inner.report_error),
            );
            let proxy = facade.proxy();
            let revision = 0;
            state.singletons.insert(
                service.id.clone(),
                SingletonBinding {
                    facade,
                    subscription: None,
                    starting: None,
                    active: Arc::clone(&active),
                    revision,
                },
            );
            state.readiness_revision += 1;
            let start_rev = if state.bound { Some(revision) } else { None };
            (proxy, start_rev, active)
        };

        if let Some(revision) = revision_to_start {
            let this = self.clone();
            let service_id = service.id.clone();
            let starting = SharedPromise::spawn(Box::pin(async move {
                let res = this.start_singleton(&service_id, revision).await;
                if let Err(ref error) = res {
                    let should_report = {
                        let state = this.inner.state.lock().unwrap();
                        !state.disposed
                            && state.bound
                            && singleton_active.load(Ordering::Acquire)
                            && state
                                .singletons
                                .get(&service_id)
                                .is_some_and(|b| b.revision == revision)
                    };
                    if should_report {
                        (this.inner.report_error)(clone_error(error));
                    }
                }
                res
            }));
            {
                let mut state = self.inner.state.lock().unwrap();
                if let Some(binding) = state.singletons.get_mut(&service.id) {
                    if binding.revision == revision {
                        binding.starting = Some(starting);
                    }
                }
            }
        }

        Ok(proxy)
    }

    pub fn observe(&self, service: &Service, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        self.assert_remotable(service)?;
        let keyed_binding = {
            let mut state = self.inner.state.lock().unwrap();
            self.assert_available_locked(&mut state, &service.id, ServiceMode::Keyed)?;
            if let Some(existing) = state.keyed.get(&service.id) {
                existing.clone()
            } else {
                let weak_inner = Arc::downgrade(&self.inner);
                let service_id = service.id.clone();
                let weak_keyed_slot: Arc<Mutex<Option<Weak<KeyedBindingInner>>>> = Arc::new(Mutex::new(None));
                let weak_slot_cb = Arc::clone(&weak_keyed_slot);
                let weak_for_access = Arc::downgrade(&self.inner);
                let assert_access: AssertAccess = Arc::new(move || {
                    let Some(inner) = weak_for_access.upgrade() else {
                        return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
                    };
                    if inner.disposed_flag.load(Ordering::Acquire) {
                        return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
                    }
                    (inner.assert_access)()
                });
                let on_empty: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    let Some(inner) = weak_inner.upgrade() else {
                        return;
                    };
                    let Some(expected_weak) = weak_slot_cb.lock().unwrap().clone() else {
                        return;
                    };
                    let Some(expected_inner) = expected_weak.upgrade() else {
                        return;
                    };
                    let removed = {
                        let mut state = inner.state.lock().unwrap();
                        let matches = state
                            .keyed
                            .get(&service_id)
                            .is_some_and(|current| Arc::ptr_eq(&current.inner, &expected_inner));
                        if !matches {
                            return;
                        }
                        state.readiness_revision += 1;
                        state.keyed.shift_remove(&service_id)
                    };
                    if let Some(binding) = removed {
                        let report_error = Arc::clone(&inner.report_error);
                        let task = async move {
                            if let Err(err) = binding.close(BACKGROUND_CONTEXT.clone()).await {
                                report_error(err);
                            }
                        };
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            handle.spawn(task);
                        } else {
                            std::thread::spawn(move || futures::executor::block_on(task));
                        }
                    }
                });

                let binding = KeyedBinding::new(
                    service.clone(),
                    self.inner.transport.clone(),
                    Arc::clone(&self.inner.report_error),
                    assert_access,
                    on_empty,
                    state.bound,
                );
                *weak_keyed_slot.lock().unwrap() = Some(Arc::downgrade(&binding.inner));
                state.keyed.insert(service.id.clone(), binding.clone());
                state.readiness_revision += 1;
                binding
            }
        };

        keyed_binding.observe(handler)
    }

    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        if self.inner.disposed_flag.load(Ordering::Acquire) {
            return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
        }
        loop {
            let (revision, starts) = {
                let state = self.inner.state.lock().unwrap();
                if state.disposed {
                    return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
                }
                let revision = state.readiness_revision;
                let transition = self.inner.binding_transition.lock().unwrap().clone();
                let mut starts: Vec<pi_js::BoxFuture<pi_js::Result<()>>> =
                    vec![Box::pin(async move { transition.wait().await })];
                for binding in state.singletons.values() {
                    if let Some(starting) = &binding.starting {
                        let starting = starting.clone();
                        starts.push(Box::pin(async move { starting.wait().await }));
                    }
                }
                for binding in state.keyed.values() {
                    starts.push(binding.ready());
                }
                (revision, starts)
            };

            await_with_context(
                async move { futures::future::try_join_all(starts).await.map(|_| ()) },
                context.clone(),
            )
            .await?;

            if self.inner.disposed_flag.load(Ordering::Acquire) {
                return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
            }
            if self.inner.state.lock().unwrap().readiness_revision == revision {
                return Ok(());
            }
        }
    }

    pub async fn rebind(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        let (singleton_jobs, keyed_bindings) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
            }
            state.bound = bound;
            self.inner.bound_flag.store(bound, Ordering::Release);
            state.readiness_revision += 1;

            let mut singleton_jobs = Vec::with_capacity(state.singletons.len());
            for (service_id, binding) in state.singletons.iter_mut() {
                binding.revision += 1;
                let facade = binding.facade.clone();
                let subscription = binding.subscription.take();
                let revision = binding.revision;
                singleton_jobs.push((service_id.clone(), facade, subscription, revision));
            }
            let keyed_bindings: Vec<KeyedBinding> = state.keyed.values().cloned().collect();
            (singleton_jobs, keyed_bindings)
        };

        let mut transition_futures: Vec<pi_js::BoxFuture<pi_js::Result<()>>> =
            Vec::with_capacity(singleton_jobs.len() + keyed_bindings.len());

        for (service_id, facade, subscription, revision) in singleton_jobs {
            facade.clear();
            let this = self.clone();
            let ctx = context.clone();
            let sid = service_id.clone();
            let promise = SharedPromise::spawn(Box::pin(async move {
                if let Some(sub) = subscription {
                    sub.close(Some(ctx)).await?;
                }
                if bound {
                    this.start_singleton(&sid, revision).await?;
                }
                Ok(())
            }));
            {
                let mut state = self.inner.state.lock().unwrap();
                if let Some(binding) = state.singletons.get_mut(&service_id) {
                    if binding.revision == revision {
                        binding.starting = Some(promise.clone());
                    }
                }
            }
            transition_futures.push(Box::pin(async move { promise.wait().await }));
        }

        for binding in keyed_bindings {
            let ctx = context.clone();
            transition_futures.push(Box::pin(async move { binding.rebind(bound, ctx).await }));
        }

        let completion = SharedPromise::spawn(Box::pin(async move {
            let results = futures::future::join_all(transition_futures).await;
            let errors: Vec<pi_js::Error> = results.into_iter().filter_map(Result::err).collect();
            if !errors.is_empty() {
                return Err(pi_js::Error::typed(
                    "AggregateError",
                    AggregateError {
                        message: "Failed to rebind services".to_string(),
                        errors,
                    },
                ));
            }
            Ok(())
        }));

        *self.inner.binding_transition.lock().unwrap() = completion.clone();
        completion.wait().await
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        let (singletons, keyed) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Ok(());
            }
            state.disposed = true;
            self.inner.disposed_flag.store(true, Ordering::Release);
            let singletons = std::mem::take(&mut state.singletons);
            let keyed = std::mem::take(&mut state.keyed);
            (singletons, keyed)
        };

        let mut closes: Vec<pi_js::BoxFuture<pi_js::Result<()>>> = Vec::new();
        for binding in singletons.into_values() {
            binding.active.store(false, Ordering::Release);
            binding.facade.clear();
            if let Some(starting) = binding.starting {
                closes.push(Box::pin(async move {
                    let _ = starting.wait().await;
                    Ok(())
                }));
            }
            if let Some(subscription) = binding.subscription {
                let ctx = context.clone();
                closes.push(Box::pin(async move { subscription.close(Some(ctx)).await }));
            }
        }
        for binding in keyed.into_values() {
            let ctx = context.clone();
            closes.push(Box::pin(async move { binding.close(ctx).await }));
        }

        let results = futures::future::join_all(closes).await;
        let errors: Vec<pi_js::Error> = results.into_iter().filter_map(Result::err).collect();
        if !errors.is_empty() {
            return Err(pi_js::Error::typed(
                "AggregateError",
                AggregateError {
                    message: "Failed to dispose services".to_string(),
                    errors,
                },
            ));
        }
        Ok(())
    }

    // PORT: looks up the singleton binding by id so this future does not borrow the state mutex across awaits.
    async fn start_singleton(&self, service_id: &str, revision: i64) -> pi_js::Result<()> {
        let weak_inner = Arc::downgrade(&self.inner);
        let sid = service_id.to_string();
        let listener = Arc::new(move |update: ServiceProviderUpdate, context: Context| {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let facade_opt = {
                let state = inner.state.lock().unwrap();
                let Some(binding) = state.singletons.get(&sid) else {
                    return;
                };
                if !binding.active.load(Ordering::Acquire) || binding.revision != revision {
                    return;
                }
                binding.facade.clone()
            };
            let outcome: pi_js::Result<()> = match update {
                ServiceProviderUpdate::Reset { snapshot } => {
                    validate_reset_snapshot(&snapshot, &sid, ServiceMode::Singleton).and_then(|()| {
                        match snapshot.instances.into_iter().next() {
                            None => {
                                facade_opt.clear();
                                Ok(())
                            }
                            Some(inst) => facade_opt.install(inst, context),
                        }
                    })
                }
                ServiceProviderUpdate::Unavailable => {
                    facade_opt.clear();
                    Ok(())
                }
                ServiceProviderUpdate::Replaced { snapshot } => {
                    if snapshot.instance.is_some() {
                        Err(pi_js::Error::js(
                            "Error",
                            "Singleton replacement has an instance address",
                        ))
                    } else {
                        facade_opt.install(snapshot, context)
                    }
                }
                ServiceProviderUpdate::State {
                    instance,
                    member,
                    sequence,
                    ops,
                } if instance.is_none() => facade_opt.update(&member, sequence, &ops, context),
                ServiceProviderUpdate::State { .. }
                | ServiceProviderUpdate::Spawned { .. }
                | ServiceProviderUpdate::Closed { .. } => Ok(()),
            };
            if let Err(error) = outcome {
                (inner.report_error)(error);
            }
        });

        let subscription = self
            .inner
            .transport
            .subscribe(service_id, ServiceMode::Singleton, listener, &BACKGROUND_CONTEXT)
            .await?;
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::yield_now().await;
        }

        let facade = {
            let mut state = self.inner.state.lock().unwrap();
            let valid = !state.disposed
                && state.bound
                && state
                    .singletons
                    .get(service_id)
                    .is_some_and(|b| b.active.load(Ordering::Acquire) && b.revision == revision);
            if !valid {
                None
            } else {
                let binding = state.singletons.get_mut(service_id).unwrap();
                binding.subscription = Some(subscription.clone());
                Some(binding.facade.clone())
            }
        };

        let Some(facade) = facade else {
            let _ = subscription.close(Some(BACKGROUND_CONTEXT.clone())).await;
            return Ok(());
        };

        let snapshot = subscription.snapshot();
        if snapshot.mode != ServiceMode::Singleton || snapshot.service_id != service_id || snapshot.instances.len() != 1
        {
            return Err(pi_js::Error::js(
                "Error",
                format!("Remote service {service_id} returned an invalid singleton snapshot"),
            ));
        }
        facade.install(snapshot.instances[0].clone(), service_delivery_context())?;
        subscription.activate()?;
        Ok(())
    }

    fn assert_remotable(&self, service: &Service) -> pi_js::Result<()> {
        if service.local {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Service {} is process-local", service.id),
            )
            .into_js_error());
        }
        Ok(())
    }

    fn assert_available_locked(
        &self,
        state: &mut BindingState,
        service_id: &str,
        mode: ServiceMode,
    ) -> pi_js::Result<()> {
        if state.disposed {
            return Err(pi_js::Error::js("Error", "Remote service binding is disposed"));
        }
        if !self.inner.allowlist.contains(service_id) {
            return Err(RemoteServiceError::new(
                RemoteServiceErrorCode::ServiceNotAllowed,
                format!("Remote service {service_id} is not allowlisted"),
            )
            .into_js_error());
        }
        if let Some(&existing) = state.modes.get(service_id) {
            if existing != mode {
                return Err(RemoteServiceError::new(
                    RemoteServiceErrorCode::ServiceModeMismatch,
                    format!("Remote service {service_id} is already used as {}", mode_str(existing)),
                )
                .into_js_error());
            }
        }
        state.modes.insert(service_id.to_string(), mode);
        Ok(())
    }
}

fn validate_reset_snapshot(
    snapshot: &ServiceSubscriptionSnapshot,
    service_id: &str,
    mode: ServiceMode,
) -> pi_js::Result<()> {
    if snapshot.service_id != service_id
        || snapshot.mode != mode
        || (mode == ServiceMode::Singleton && snapshot.instances.len() > 1)
    {
        return Err(pi_js::Error::js(
            "Error",
            "Remote service reset has the wrong service or mode",
        ));
    }
    let mut keys = IndexSet::new();
    for instance in &snapshot.instances {
        let valid_addr = match mode {
            ServiceMode::Singleton => instance.instance.is_none(),
            ServiceMode::Keyed => instance.instance.is_some(),
        };
        if !valid_addr {
            return Err(pi_js::Error::js(
                "Error",
                "Remote service reset has an invalid instance address",
            ));
        }
        if let Some(addr) = &instance.instance {
            if !keys.insert(addr.key.clone()) {
                return Err(pi_js::Error::js(
                    "Error",
                    "Remote service reset repeats an instance key",
                ));
            }
        }
        for member in &instance.members {
            if let ServiceMemberSnapshot::State(state_snap) = member {
                let valid = state_snap.ops.len() == 1 && matches!(state_snap.ops.first(), Some(Op::Replace(_)));
                if !valid {
                    return Err(pi_js::Error::js(
                        "Error",
                        "Remote service reset must contain full root replacements",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_members(members: &[ServiceMemberSnapshot]) -> pi_js::Result<IndexMap<String, ServiceMemberSnapshot>> {
    let mut result = IndexMap::new();
    for member in members {
        let name = match member {
            ServiceMemberSnapshot::Method(m) => &m.name,
            ServiceMemberSnapshot::State(s) => &s.name,
        };
        if name.is_empty() || result.contains_key(name) {
            return Err(pi_js::Error::js(
                "Error",
                "Remote service has invalid member descriptions",
            ));
        }
        result.insert(name.clone(), member.clone());
    }
    Ok(result)
}

fn same_address(left: Option<&ServiceInstanceAddress>, right: Option<&ServiceInstanceAddress>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => a.key == b.key && a.generation == b.generation,
        _ => false,
    }
}

fn kind_str(kind: ServiceMemberKind) -> &'static str {
    match kind {
        ServiceMemberKind::Method => "method",
        ServiceMemberKind::State => "state",
    }
}

fn mode_str(mode: ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Singleton => "singleton",
        ServiceMode::Keyed => "keyed",
    }
}

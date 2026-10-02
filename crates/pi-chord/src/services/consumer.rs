//! Port of packages/chord/src/services/consumer.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

use crate::delta::Op;
use crate::services::handle::{ServiceObject, ServiceTarget, ServiceValue};
use crate::services::instances::{InstanceDirectory, InstanceEntry};
use crate::services::state::ReplicatedStateReplica;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum ServiceMemberKind {
    #[serde(rename = "method")]
    Method,
    #[serde(rename = "state")]
    State,
}

// PORT: a JS promise settles once and can be awaited any number of times.
struct SharedPromise {
    inner: Arc<SharedPromiseInner>,
}

struct SharedPromiseInner {
    future: Mutex<Option<pi_js::BoxFuture<pi_js::Result<()>>>>,
    outcome: Mutex<Option<std::result::Result<(), Arc<pi_js::Error>>>>,
}

impl SharedPromise {
    fn from_future(future: pi_js::BoxFuture<pi_js::Result<()>>) -> Self {
        Self {
            inner: Arc::new(SharedPromiseInner {
                future: Mutex::new(Some(future)),
                outcome: Mutex::new(None),
            }),
        }
    }
}

impl Clone for SharedPromise {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct MemberSlot {
    inner: Arc<MemberSlotInner>,
}

struct MemberSlotInner {
    service_id: String,
    member: String,
    invoke: InvokeFn,
    state: ReplicatedStateReplica,
    is_active: IsActive,
    assert_access: AssertAccess,
    value: ServiceObject,
    kind: Mutex<Option<ServiceMemberKind>>,
    expected_kind: Mutex<Option<ServiceMemberKind>>,
}

impl Clone for MemberSlot {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
        todo!("port: MemberSlot::new")
    }

    fn set_description(&self, kind: ServiceMemberKind) -> pi_js::Result<()> {
        todo!("port: MemberSlot::set_description")
    }

    fn hydrate(&self, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        todo!("port: MemberSlot::hydrate")
    }

    fn update(&self, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        todo!("port: MemberSlot::update")
    }

    fn clear(&self) {
        todo!("port: MemberSlot::clear")
    }

    fn subscribe(&self, listener: StateListener) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: MemberSlot::subscribe")
    }

    fn expect(&self, kind: ServiceMemberKind) -> pi_js::Result<()> {
        todo!("port: MemberSlot::expect")
    }

    // PORT: `#call` is synchronous and returns a Promise.
    fn call(&self, args: &[ServiceValue]) -> pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>> {
        todo!("port: MemberSlot::call")
    }
}

impl ServiceTarget for MemberSlot {
    fn get(&self, property: &crate::services::handle::PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: MemberSlot::get")
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        todo!("port: MemberSlot::apply")
    }
}

struct ServiceFacade {
    inner: Arc<ServiceFacadeInner>,
}

struct ServiceFacadeInner {
    service_id: String,
    address: Option<ServiceInstanceAddress>,
    transport: RemoteServiceTransport,
    report_error: ErrorReporter,
    slots: Mutex<IndexMap<String, MemberSlot>>,
    descriptions: Mutex<IndexMap<String, ServiceMemberKind>>,
    is_active: IsActive,
    assert_access: AssertAccess,
    proxy: ServiceObject,
}

impl Clone for ServiceFacade {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
        todo!("port: ServiceFacade::new")
    }

    fn install(&self, snapshot: ServiceInstanceSnapshot, context: Context) -> pi_js::Result<()> {
        todo!("port: ServiceFacade::install")
    }

    fn update(&self, member: &str, sequence: i64, ops: &[Op], context: Context) -> pi_js::Result<()> {
        todo!("port: ServiceFacade::update")
    }

    fn clear(&self) {
        todo!("port: ServiceFacade::clear")
    }

    fn slot(&self, member: &str) -> pi_js::Result<MemberSlot> {
        todo!("port: ServiceFacade::slot")
    }
}

impl ServiceTarget for ServiceFacade {
    fn get(&self, property: &crate::services::handle::PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: ServiceFacade::get")
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        todo!("port: ServiceFacade::apply")
    }
}

struct SingletonBinding {
    facade: ServiceFacade,
    subscription: Option<ServiceSubscription>,
    starting: Option<SharedPromise>,
    active: bool,
    revision: i64,
}

struct KeyedInstance {
    key: String,
    generation: i64,
    service: ServiceObject,
    deactivate: Arc<dyn Fn() + Send + Sync>,
    facade: ServiceFacade,
}

impl Clone for KeyedInstance {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            generation: self.generation,
            service: self.service.clone(),
            deactivate: Arc::clone(&self.deactivate),
            facade: self.facade.clone(),
        }
    }
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

impl Clone for KeyedBinding {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
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
        todo!("port: KeyedBinding::new")
    }

    fn observe(&self, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: KeyedBinding::observe")
    }

    async fn rebind(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        todo!("port: KeyedBinding::rebind")
    }

    // PORT: `ready` is synchronous and returns a Promise.
    fn ready(&self) -> pi_js::BoxFuture<pi_js::Result<()>> {
        todo!("port: KeyedBinding::ready")
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: KeyedBinding::close")
    }

    async fn reset(&self, context: Context, wait_for_starting: bool) -> pi_js::Result<()> {
        todo!("port: KeyedBinding::reset")
    }

    async fn start(&self, revision: i64) -> pi_js::Result<()> {
        todo!("port: KeyedBinding::start")
    }

    fn update(&self, update: ServiceProviderUpdate, context: Context) {
        todo!("port: KeyedBinding::update")
    }

    fn spawn(&self, snapshot: ServiceInstanceSnapshot, context: Context) -> pi_js::Result<()> {
        todo!("port: KeyedBinding::spawn")
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
    state: Mutex<BindingState>,
    binding_transition: Mutex<SharedPromise>,
}

impl RemoteServiceBindingImpl {
    pub fn new(options: RemoteServiceBindingOptions) -> pi_js::Result<Self> {
        todo!("port: RemoteServiceBindingImpl::new")
    }

    pub fn r#use(&self, service: &Service) -> pi_js::Result<ServiceObject> {
        todo!("port: RemoteServiceBindingImpl::use")
    }

    pub fn observe(&self, service: &Service, handler: ObserveHandler) -> pi_js::Result<pi_js::Unsubscribe> {
        todo!("port: RemoteServiceBindingImpl::observe")
    }

    pub async fn ready(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::ready")
    }

    pub async fn rebind(&self, bound: bool, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::rebind")
    }

    pub async fn dispose(&self, context: Context) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::dispose")
    }

    // PORT: the singleton binding is looked up by id so this future does not borrow the state mutex.
    async fn start_singleton(&self, service_id: &str, revision: i64) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::start_singleton")
    }

    fn assert_handle_access(&self) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::assert_handle_access")
    }

    fn assert_remotable(&self, service: &Service) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::assert_remotable")
    }

    fn assert_available(&self, service_id: &str, mode: ServiceMode) -> pi_js::Result<()> {
        todo!("port: RemoteServiceBindingImpl::assert_available")
    }
}

fn validate_reset_snapshot(
    snapshot: &ServiceSubscriptionSnapshot,
    service_id: &str,
    mode: ServiceMode,
) -> pi_js::Result<()> {
    todo!("port: validate_reset_snapshot")
}

fn validate_members(members: &[ServiceMemberSnapshot]) -> pi_js::Result<IndexMap<String, ServiceMemberSnapshot>> {
    todo!("port: validate_members")
}

fn same_address(left: Option<&ServiceInstanceAddress>, right: Option<&ServiceInstanceAddress>) -> bool {
    todo!("port: same_address")
}

fn is_context(value: &ServiceValue) -> bool {
    todo!("port: is_context")
}

fn to_error(error: Box<dyn std::error::Error + Send + Sync>) -> pi_js::Error {
    todo!("port: to_error")
}

//! Port of packages/chord/src/services/handle.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

// PORT: JS Proxy traps are methods on [`ServiceTarget`]. Numeric property keys are their decimal strings.
// Well-known symbols use their description (`toStringTag`).

/// JS property key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyKey {
    String(String),
    Symbol(String),
}

/// Runtime value carried by a service proxy.
#[derive(Clone)]
pub enum ServiceValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Object(ServiceObject),
}

pub struct ResolvedValue {
    pub value: ServiceValue,
    pub receiver: ServiceObject,
}

/// Object or function targeted by a service proxy.
pub trait ServiceTarget: Send + Sync {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue>;
    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue>;
}

/// Type-erased service implementation or proxy (`object` / `Service<T>`'s `T`).
#[derive(Clone)]
pub struct ServiceObject {
    inner: Arc<dyn ServiceTarget>,
}

impl ServiceObject {
    pub fn new(target: Arc<dyn ServiceTarget>) -> Self {
        Self { inner: target }
    }

    pub fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        self.inner.get(property)
    }

    pub fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        self.inner.apply(this_arg, args)
    }
}

type ValueResolver = Arc<dyn Fn() -> pi_js::Result<ResolvedValue> + Send + Sync>;
type AssertAccess = Arc<dyn Fn() -> pi_js::Result<()> + Send + Sync>;

/// Host-owned mutable target with consumer-owned guarded views.
#[derive(Clone)]
pub struct ServiceSlot {
    inner: Arc<ServiceSlotInner>,
}

struct ServiceSlotInner {
    service_id: String,
    wrap_objects: bool,
    implementation: Mutex<Option<ServiceObject>>,
}

impl ServiceSlot {
    pub fn new(service_id: &str, wrap_objects: bool) -> Self {
        Self {
            inner: Arc::new(ServiceSlotInner {
                service_id: service_id.to_string(),
                wrap_objects,
                implementation: Mutex::new(None),
            }),
        }
    }

    pub fn view(&self, assert_access: AssertAccess) -> ServiceObject {
        todo!("port: ServiceSlot::view")
    }

    pub fn bind(&self, implementation: ServiceObject) {
        todo!("port: ServiceSlot::bind")
    }

    pub fn unbind(&self) {
        todo!("port: ServiceSlot::unbind")
    }

    pub fn resolve(
        &self,
        property: &PropertyKey,
        assert_access: &dyn Fn() -> pi_js::Result<()>,
    ) -> pi_js::Result<ResolvedValue> {
        todo!("port: ServiceSlot::resolve")
    }
}

struct ServiceView {
    inner: Arc<ServiceViewInner>,
}

struct ServiceViewInner {
    slot: ServiceSlot,
    assert_access: AssertAccess,
    wrap_objects: bool,
    members: Mutex<IndexMap<PropertyKey, ValueView>>,
    proxy: ServiceObject,
}

impl ServiceView {
    fn new(slot: ServiceSlot, assert_access: AssertAccess, wrap_objects: bool) -> Self {
        todo!("port: ServiceView::new")
    }

    fn get_member(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: ServiceView::get_member")
    }
}

impl ServiceTarget for ServiceView {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: ServiceView::get")
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        todo!("port: ServiceView::apply")
    }
}

struct ValueView {
    inner: Arc<ValueViewInner>,
}

struct ValueViewInner {
    resolve: ValueResolver,
    children: Mutex<IndexMap<PropertyKey, ValueView>>,
    // PORT: remembers the Proxy target's [[Call]] slot, which is not a TS field.
    callable: bool,
    proxy: ServiceObject,
}

impl ValueView {
    fn new(resolve: ValueResolver, callable: bool) -> Self {
        todo!("port: ValueView::new")
    }

    fn invoke(&self, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        todo!("port: ValueView::invoke")
    }

    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: ValueView::get")
    }
}

impl ServiceTarget for ValueView {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        todo!("port: ValueView::get")
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        todo!("port: ValueView::apply")
    }
}

fn is_object(value: &ServiceValue) -> bool {
    todo!("port: is_object")
}

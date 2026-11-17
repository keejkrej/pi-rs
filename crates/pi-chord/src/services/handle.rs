//! Port of packages/chord/src/services/handle.ts

#![allow(dead_code, unused_variables)]

use std::any::Any;
use std::future::Future;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use crate::services::errors::clone_error;
use crate::services::state_internals::ReplicatedStateInternals;
use crate::types::{Context, JsonValue};

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

impl ServiceValue {
    pub fn from_json(value: &JsonValue) -> Self {
        match value {
            JsonValue::Null => ServiceValue::Null,
            JsonValue::Bool(b) => ServiceValue::Bool(*b),
            JsonValue::Number(n) => ServiceValue::Number(n.as_f64().unwrap_or(0.0)),
            JsonValue::String(s) => ServiceValue::String(s.clone()),
            JsonValue::Array(_) | JsonValue::Object(_) => ServiceValue::Object(ServiceObject::from_json(value.clone())),
        }
    }

    pub fn to_json(&self) -> pi_js::Result<JsonValue> {
        match self {
            ServiceValue::Undefined => Err(pi_js::Error::js("TypeError", "Cannot convert undefined to JSON")),
            ServiceValue::Null => Ok(JsonValue::Null),
            ServiceValue::Bool(b) => Ok(JsonValue::Bool(*b)),
            ServiceValue::Number(n) => {
                if !n.is_finite() {
                    Ok(JsonValue::Null)
                } else if n.fract() == 0.0 && *n >= (i64::MIN as f64) && *n <= (i64::MAX as f64) {
                    Ok(JsonValue::Number((*n as i64).into()))
                } else {
                    Ok(serde_json::Number::from_f64(*n)
                        .map(JsonValue::Number)
                        .unwrap_or(JsonValue::Null))
                }
            }
            ServiceValue::String(s) => Ok(JsonValue::String(s.clone())),
            ServiceValue::Object(obj) => obj
                .to_json()
                .ok_or_else(|| pi_js::Error::js("TypeError", "Service value object is not JSON serializable")),
        }
    }

    pub async fn resolve_json(self) -> pi_js::Result<Option<JsonValue>> {
        match self {
            ServiceValue::Undefined => Ok(None),
            ServiceValue::Object(obj) => {
                if let Some(future) = obj.into_json_future() {
                    return future.await;
                }
                Ok(Some(ServiceValue::Object(obj).to_json()?))
            }
            other => Ok(Some(other.to_json()?)),
        }
    }

    pub fn into_object(self) -> pi_js::Result<ServiceObject> {
        match self {
            ServiceValue::Object(obj) => Ok(obj),
            _ => Err(pi_js::Error::js("TypeError", "Service value is not an object")),
        }
    }
}

pub struct ResolvedValue {
    pub value: ServiceValue,
    pub receiver: ServiceObject,
}

/// Object or function targeted by a service proxy.
pub trait ServiceTarget: Send + Sync {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue>;
    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue>;

    fn is_callable(&self) -> bool {
        false
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        None
    }

    fn keys(&self) -> Option<Vec<String>> {
        None
    }

    fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        None
    }

    fn to_json(&self) -> Option<JsonValue> {
        None
    }

    fn as_context(&self) -> Option<Context> {
        None
    }

    fn take_future(&self) -> Option<pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>>> {
        None
    }
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

    pub fn from_fn<F>(func: F) -> Self
    where
        F: Fn(Option<&ServiceObject>, &[ServiceValue]) -> pi_js::Result<ServiceValue> + Send + Sync + 'static,
    {
        Self::new(Arc::new(FnTarget(Arc::new(func))))
    }

    pub fn from_promise<Fut>(future: Fut) -> Self
    where
        Fut: Future<Output = pi_js::Result<Option<JsonValue>>> + Send + 'static,
    {
        Self::new(Arc::new(PromiseTarget {
            future: Mutex::new(Some(Box::pin(future))),
            outcome: Mutex::new(None),
        }))
    }

    pub fn from_json(value: JsonValue) -> Self {
        Self::new(Arc::new(JsonTarget(value)))
    }

    pub fn from_context(context: Context) -> Self {
        Self::new(Arc::new(ContextTarget(context)))
    }

    pub fn from_members(members: IndexMap<String, ServiceValue>) -> Self {
        Self::new(Arc::new(MapTarget(members)))
    }

    pub fn from_state<T>(state: T, internals: Arc<dyn ReplicatedStateInternals>) -> Self
    where
        T: ServiceTarget + 'static,
    {
        Self::new(Arc::new(StateTarget {
            target: state,
            internals,
        }))
    }

    pub fn target(&self) -> &Arc<dyn ServiceTarget> {
        &self.inner
    }

    pub fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        self.inner.get(property)
    }

    pub fn get_key(&self, key: &str) -> pi_js::Result<ServiceValue> {
        self.inner.get(&PropertyKey::String(key.to_string()))
    }

    pub fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        self.inner.apply(this_arg, args)
    }

    pub fn is_callable(&self) -> bool {
        self.inner.is_callable()
    }

    pub fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.inner.as_any()
    }

    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.as_any()?.downcast_ref::<T>()
    }

    pub fn keys(&self) -> Option<Vec<String>> {
        self.inner.keys()
    }

    pub fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        self.inner.state_internals()
    }

    pub fn to_json(&self) -> Option<JsonValue> {
        self.inner.to_json()
    }

    pub fn as_context(&self) -> Option<Context> {
        self.inner.as_context()
    }

    pub fn into_json_future(&self) -> Option<pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>>> {
        self.inner.take_future()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        let a = Arc::as_ptr(&self.inner) as *const ();
        let b = Arc::as_ptr(&other.inner) as *const ();
        std::ptr::eq(a, b)
    }
}

struct FnTarget(Arc<dyn Fn(Option<&ServiceObject>, &[ServiceValue]) -> pi_js::Result<ServiceValue> + Send + Sync>);

impl ServiceTarget for FnTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        (self.0)(this_arg, args)
    }

    fn is_callable(&self) -> bool {
        true
    }
}

struct PromiseTarget {
    future: Mutex<Option<pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>>>>,
    outcome: Mutex<Option<std::result::Result<Option<JsonValue>, Arc<pi_js::Error>>>>,
}

impl ServiceTarget for PromiseTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "Promise is not a function"))
    }

    fn take_future(&self) -> Option<pi_js::BoxFuture<pi_js::Result<Option<JsonValue>>>> {
        if let Some(outcome) = self.outcome.lock().unwrap().as_ref() {
            let cloned = match outcome {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(clone_error(e)),
            };
            return Some(Box::pin(async move { cloned }));
        }
        self.future.lock().unwrap().take()
    }
}

struct ContextTarget(Context);

impl ServiceTarget for ContextTarget {
    fn get(&self, _property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        Ok(ServiceValue::Undefined)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "Context is not a function"))
    }

    fn as_context(&self) -> Option<Context> {
        Some(self.0.clone())
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(&self.0)
    }
}

struct JsonTarget(JsonValue);

impl ServiceTarget for JsonTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let PropertyKey::String(key) = property else {
            return Ok(ServiceValue::Undefined);
        };
        match &self.0 {
            JsonValue::Object(map) => Ok(map
                .get(key)
                .map(ServiceValue::from_json)
                .unwrap_or(ServiceValue::Undefined)),
            JsonValue::Array(arr) => {
                if key == "length" {
                    return Ok(ServiceValue::Number(arr.len() as f64));
                }
                if let Ok(index) = key.parse::<usize>() {
                    if index.to_string() == *key {
                        return Ok(arr
                            .get(index)
                            .map(ServiceValue::from_json)
                            .unwrap_or(ServiceValue::Undefined));
                    }
                }
                Ok(ServiceValue::Undefined)
            }
            _ => Ok(ServiceValue::Undefined),
        }
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "JSON value is not callable"))
    }

    fn keys(&self) -> Option<Vec<String>> {
        match &self.0 {
            JsonValue::Object(map) => Some(map.keys().cloned().collect()),
            JsonValue::Array(arr) => Some((0..arr.len()).map(|i| i.to_string()).collect()),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<JsonValue> {
        Some(self.0.clone())
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        Some(&self.0)
    }
}

struct MapTarget(IndexMap<String, ServiceValue>);

impl ServiceTarget for MapTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let PropertyKey::String(key) = property else {
            return Ok(ServiceValue::Undefined);
        };
        Ok(self.0.get(key).cloned().unwrap_or(ServiceValue::Undefined))
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "Service target is not callable"))
    }

    fn keys(&self) -> Option<Vec<String>> {
        Some(self.0.keys().cloned().collect())
    }
}

struct StateTarget<T> {
    target: T,
    internals: Arc<dyn ReplicatedStateInternals>,
}

impl<T: ServiceTarget + 'static> ServiceTarget for StateTarget<T> {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        self.target.get(property)
    }

    fn apply(&self, this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        self.target.apply(this_arg, args)
    }

    fn is_callable(&self) -> bool {
        self.target.is_callable()
    }

    fn as_any(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.target.as_any()
    }

    fn keys(&self) -> Option<Vec<String>> {
        self.target.keys()
    }

    fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        Some(Arc::clone(&self.internals))
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
        ServiceObject::new(Arc::new(ServiceViewTarget(Arc::new(ServiceViewInner {
            slot: self.clone(),
            assert_access,
            wrap_objects: self.inner.wrap_objects,
            members: Mutex::new(IndexMap::new()),
        }))))
    }

    pub fn bind(&self, implementation: ServiceObject) {
        *self.inner.implementation.lock().unwrap() = Some(implementation);
    }

    pub fn unbind(&self) {
        *self.inner.implementation.lock().unwrap() = None;
    }

    pub fn resolve(
        &self,
        property: &PropertyKey,
        assert_access: &dyn Fn() -> pi_js::Result<()>,
    ) -> pi_js::Result<ResolvedValue> {
        assert_access()?;
        let implementation = self.inner.implementation.lock().unwrap().clone();
        let Some(implementation) = implementation else {
            return Err(pi_js::Error::msg(format!(
                "Service {} is disconnected",
                self.inner.service_id
            )));
        };
        let value = implementation.get(property)?;
        Ok(ResolvedValue {
            value,
            receiver: implementation,
        })
    }
}

struct ServiceViewTarget(Arc<ServiceViewInner>);

struct ServiceViewInner {
    slot: ServiceSlot,
    assert_access: AssertAccess,
    wrap_objects: bool,
    members: Mutex<IndexMap<PropertyKey, ServiceObject>>,
}

impl ServiceViewTarget {
    fn get_member(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let slot = self.0.slot.clone();
        let assert_access = Arc::clone(&self.0.assert_access);
        let prop = property.clone();
        let resolve: ValueResolver = Arc::new(move || slot.resolve(&prop, &*assert_access));
        let current = resolve()?.value;
        let current_callable = matches!(&current, ServiceValue::Object(obj) if obj.is_callable());
        if !is_object(&current) || (!current_callable && !self.0.wrap_objects) {
            return Ok(current);
        }
        let mut members = self.0.members.lock().unwrap();
        if let Some(member) = members.get(property) {
            return Ok(ServiceValue::Object(member.clone()));
        }
        let proxy = ServiceObject::new(Arc::new(ValueViewTarget(Arc::new(ValueViewInner {
            resolve,
            children: Mutex::new(IndexMap::new()),
            callable: current_callable,
        }))));
        members.insert(property.clone(), proxy.clone());
        Ok(ServiceValue::Object(proxy))
    }
}

impl ServiceTarget for ServiceViewTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        self.get_member(property)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, _args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        Err(pi_js::Error::js("TypeError", "Service proxy is not callable"))
    }

    fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        if (self.0.assert_access)().is_err() {
            return None;
        }
        let implementation = self.0.slot.inner.implementation.lock().unwrap().clone()?;
        implementation.state_internals()
    }
}

struct ValueViewTarget(Arc<ValueViewInner>);

struct ValueViewInner {
    resolve: ValueResolver,
    children: Mutex<IndexMap<PropertyKey, ServiceObject>>,
    // PORT: remembers the Proxy target's [[Call]] slot, which is not a TS field.
    callable: bool,
}

impl ValueViewTarget {
    fn invoke(&self, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        let ResolvedValue { value, receiver } = (self.0.resolve)()?;
        match value {
            ServiceValue::Object(obj) if obj.is_callable() => obj.apply(Some(&receiver), args),
            _ => Err(pi_js::Error::js("TypeError", "Service member is not callable")),
        }
    }

    fn get_child(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        let parent_resolve = Arc::clone(&self.0.resolve);
        let prop = property.clone();
        let resolve: ValueResolver = Arc::new(move || {
            let parent = (parent_resolve)()?;
            let ServiceValue::Object(parent_obj) = parent.value else {
                return Err(pi_js::Error::js("TypeError", "Service member does not have properties"));
            };
            let value = parent_obj.get(&prop)?;
            Ok(ResolvedValue {
                value,
                receiver: parent_obj,
            })
        });
        let current = resolve()?.value;
        let current_callable = matches!(&current, ServiceValue::Object(obj) if obj.is_callable());
        if !current_callable {
            return Ok(current);
        }
        let mut children = self.0.children.lock().unwrap();
        if let Some(child) = children.get(property) {
            return Ok(ServiceValue::Object(child.clone()));
        }
        let proxy = ServiceObject::new(Arc::new(ValueViewTarget(Arc::new(ValueViewInner {
            resolve,
            children: Mutex::new(IndexMap::new()),
            callable: true,
        }))));
        children.insert(property.clone(), proxy.clone());
        Ok(ServiceValue::Object(proxy))
    }
}

impl ServiceTarget for ValueViewTarget {
    fn get(&self, property: &PropertyKey) -> pi_js::Result<ServiceValue> {
        self.get_child(property)
    }

    fn apply(&self, _this_arg: Option<&ServiceObject>, args: &[ServiceValue]) -> pi_js::Result<ServiceValue> {
        self.invoke(args)
    }

    fn is_callable(&self) -> bool {
        self.0.callable
    }

    fn state_internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        let resolved = (self.0.resolve)().ok()?;
        match resolved.value {
            ServiceValue::Object(obj) => obj.state_internals(),
            _ => None,
        }
    }
}

fn is_object(value: &ServiceValue) -> bool {
    matches!(value, ServiceValue::Object(_))
}

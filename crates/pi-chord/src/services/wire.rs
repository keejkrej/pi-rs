//! Port of packages/chord/src/services/wire.ts

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

const SERVICE_CONTROL_ID: &str = "$chord.service";
const SERVICE_CATALOGUE_MEMBER: &str = "catalogue";
const SERVICE_SUBSCRIBE_MEMBER: &str = "subscribe";
const SERVICE_UNSUBSCRIBE_MEMBER: &str = "unsubscribe";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireServiceMethodKind {
    #[serde(rename = "method")]
    Method,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireServiceStateKind {
    #[serde(rename = "state")]
    State,
}

/// Method arm of [`WireServiceMemberSnapshot`]. Key order is `name`, `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServiceMethodSnapshot {
    pub name: String,
    pub kind: WireServiceMethodKind,
}

/// State arm of [`WireServiceMemberSnapshot`]. Key order is `name`, `kind`, `sequence`, `ops`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServiceStateSnapshot {
    pub name: String,
    pub kind: WireServiceStateKind,
    pub sequence: i64,
    pub ops: Vec<crate::delta::WireOp>,
}

// PORT: writers emit `name` before `kind`. `#[serde(tag = "kind")]` would emit `kind` first,
// so the union is untagged and each arm carries its kind field in literal order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub enum WireServiceMemberSnapshot {
    Method(WireServiceMethodSnapshot),
    State(WireServiceStateSnapshot),
}

/// Key order is `instance`, `members` (instance omitted when absent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServiceInstanceSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<crate::types::ServiceInstanceAddress>,
    pub members: Vec<WireServiceMemberSnapshot>,
}

/// Key order is `serviceId`, `mode`, `instances`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServiceSubscriptionSnapshot {
    pub service_id: String,
    pub mode: crate::types::ServiceMode,
    pub instances: Vec<WireServiceInstanceSnapshot>,
}

/// Key order follows the object literals in `provider.ts` / `state-codec.ts`.
/// `type` is first on every arm, then the remaining keys in literal order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum WireServiceProviderUpdate {
    #[serde(rename = "state")]
    State {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance: Option<crate::types::ServiceInstanceAddress>,
        member: String,
        sequence: i64,
        ops: Vec<crate::delta::WireOp>,
    },
    #[serde(rename = "reset")]
    Reset { snapshot: WireServiceSubscriptionSnapshot },
    #[serde(rename = "unavailable")]
    Unavailable,
    #[serde(rename = "replaced")]
    Replaced { snapshot: WireServiceInstanceSnapshot },
    #[serde(rename = "spawned")]
    Spawned { instance: WireServiceInstanceSnapshot },
    #[serde(rename = "closed")]
    Closed {
        instance: crate::types::ServiceInstanceAddress,
    },
}

/// Decoded `$chord.service` call. Not itself a JSON object; [`ServiceCall`](crate::types::ServiceCall) is the wire form.
/// Object-literal order, if serialized, is `type` then the fields below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceControlCall {
    Catalogue,
    Subscribe {
        subscription_id: String,
        service_id: String,
        mode: crate::types::ServiceMode,
    },
    Unsubscribe {
        subscription_id: String,
    },
}

pub fn create_service_catalogue_call() -> crate::types::ServiceCall {
    todo!("port: create_service_catalogue_call")
}

pub fn create_service_subscribe_call(
    subscription_id: &str,
    service_id: &str,
    mode: crate::types::ServiceMode,
) -> crate::types::ServiceCall {
    todo!("port: create_service_subscribe_call")
}

pub fn create_service_unsubscribe_call(subscription_id: &str) -> crate::types::ServiceCall {
    todo!("port: create_service_unsubscribe_call")
}

pub fn decode_service_control_call(call: &crate::types::ServiceCall) -> Option<ServiceControlCall> {
    todo!("port: decode_service_control_call")
}

pub fn parse_service_call(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceCall> {
    todo!("port: parse_service_call")
}

pub fn parse_service_catalogue(value: &serde_json::Value) -> pi_js::Result<Vec<crate::types::ServiceCatalogueEntry>> {
    todo!("port: parse_service_catalogue")
}

pub fn parse_service_subscription_snapshot(
    value: &serde_json::Value,
) -> pi_js::Result<crate::types::ServiceSubscriptionSnapshot> {
    todo!("port: parse_service_subscription_snapshot")
}

pub fn parse_wire_service_subscription_snapshot(
    value: &serde_json::Value,
) -> pi_js::Result<WireServiceSubscriptionSnapshot> {
    todo!("port: parse_wire_service_subscription_snapshot")
}

pub fn parse_service_provider_update(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceProviderUpdate> {
    todo!("port: parse_service_provider_update")
}

pub fn parse_wire_service_provider_update(value: &serde_json::Value) -> pi_js::Result<WireServiceProviderUpdate> {
    todo!("port: parse_wire_service_provider_update")
}

fn assert_subscription_snapshot(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    todo!("port: assert_subscription_snapshot")
}

fn assert_provider_update(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    todo!("port: assert_provider_update")
}

fn assert_instance(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    todo!("port: assert_instance")
}

fn assert_address(value: &serde_json::Value) -> pi_js::Result<()> {
    todo!("port: assert_address")
}

fn record<'a>(
    value: &'a serde_json::Value,
    description: &str,
) -> pi_js::Result<&'a serde_json::Map<String, serde_json::Value>> {
    todo!("port: record")
}

fn assert_keys(
    value: &serde_json::Map<String, serde_json::Value>,
    required: &[&str],
    optional: &[&str],
    description: &str,
) -> pi_js::Result<()> {
    todo!("port: assert_keys")
}

fn is_id(value: &serde_json::Value) -> bool {
    todo!("port: is_id")
}

fn is_mode(value: &serde_json::Value) -> bool {
    todo!("port: is_mode")
}

fn is_integer(value: &serde_json::Value, minimum: i64) -> bool {
    todo!("port: is_integer")
}

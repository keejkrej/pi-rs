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
    crate::types::ServiceCall {
        service_id: SERVICE_CONTROL_ID.to_string(),
        instance: None,
        member: SERVICE_CATALOGUE_MEMBER.to_string(),
        args: Vec::new(),
    }
}

pub fn create_service_subscribe_call(
    subscription_id: &str,
    service_id: &str,
    mode: crate::types::ServiceMode,
) -> crate::types::ServiceCall {
    let mode_str = match mode {
        crate::types::ServiceMode::Singleton => "singleton",
        crate::types::ServiceMode::Keyed => "keyed",
    };
    crate::types::ServiceCall {
        service_id: SERVICE_CONTROL_ID.to_string(),
        instance: None,
        member: SERVICE_SUBSCRIBE_MEMBER.to_string(),
        args: vec![
            serde_json::Value::String(subscription_id.to_string()),
            serde_json::Value::String(service_id.to_string()),
            serde_json::Value::String(mode_str.to_string()),
        ],
    }
}

pub fn create_service_unsubscribe_call(subscription_id: &str) -> crate::types::ServiceCall {
    crate::types::ServiceCall {
        service_id: SERVICE_CONTROL_ID.to_string(),
        instance: None,
        member: SERVICE_UNSUBSCRIBE_MEMBER.to_string(),
        args: vec![serde_json::Value::String(subscription_id.to_string())],
    }
}

pub fn decode_service_control_call(call: &crate::types::ServiceCall) -> Option<ServiceControlCall> {
    if call.service_id != SERVICE_CONTROL_ID || call.instance.is_some() {
        return None;
    }
    if call.member == SERVICE_CATALOGUE_MEMBER && call.args.is_empty() {
        return Some(ServiceControlCall::Catalogue);
    }
    if call.member == SERVICE_SUBSCRIBE_MEMBER && call.args.len() == 3 {
        if is_id(&call.args[0]) && is_id(&call.args[1]) && is_mode(&call.args[2]) {
            let subscription_id = call.args[0].as_str()?.to_string();
            let service_id = call.args[1].as_str()?.to_string();
            let mode = match call.args[2].as_str()? {
                "singleton" => crate::types::ServiceMode::Singleton,
                "keyed" => crate::types::ServiceMode::Keyed,
                _ => return None,
            };
            return Some(ServiceControlCall::Subscribe {
                subscription_id,
                service_id,
                mode,
            });
        }
    }
    if call.member == SERVICE_UNSUBSCRIBE_MEMBER && call.args.len() == 1 && is_id(&call.args[0]) {
        return Some(ServiceControlCall::Unsubscribe {
            subscription_id: call.args[0].as_str()?.to_string(),
        });
    }
    None
}

pub fn parse_service_call(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceCall> {
    let call = record(value, "service call")?;
    assert_keys(call, &["serviceId", "member", "args"], &["instance"], "service call")?;
    let service_id_val = call
        .get("serviceId")
        .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid service call"))?;
    let member_val = call
        .get("member")
        .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid service call"))?;
    let args_val = call
        .get("args")
        .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid service call"))?;
    if !is_id(service_id_val) || !is_id(member_val) || !args_val.is_array() {
        return Err(pi_js::Error::js("TypeError", "Invalid service call"));
    }
    let instance = match call.get("instance") {
        Some(inst) => Some(assert_address(inst)?),
        None => None,
    };
    Ok(crate::types::ServiceCall {
        service_id: service_id_val.as_str().unwrap().to_string(),
        instance,
        member: member_val.as_str().unwrap().to_string(),
        args: args_val.as_array().unwrap().clone(),
    })
}

pub fn parse_service_catalogue(value: &serde_json::Value) -> pi_js::Result<Vec<crate::types::ServiceCatalogueEntry>> {
    let Some(arr) = value.as_array() else {
        return Err(pi_js::Error::js("TypeError", "Invalid service catalogue"));
    };
    let mut ids = indexmap::IndexSet::new();
    let mut out = Vec::with_capacity(arr.len());
    for candidate in arr {
        let entry = record(candidate, "service catalogue entry")?;
        assert_keys(entry, &["serviceId", "mode"], &[], "service catalogue entry")?;
        let service_id_val = entry
            .get("serviceId")
            .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid service catalogue"))?;
        let mode_val = entry
            .get("mode")
            .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid service catalogue"))?;
        if !is_id(service_id_val) || !is_mode(mode_val) {
            return Err(pi_js::Error::js("TypeError", "Invalid service catalogue"));
        }
        let service_id = service_id_val.as_str().unwrap().to_string();
        if !ids.insert(service_id.clone()) {
            return Err(pi_js::Error::js("TypeError", "Invalid service catalogue"));
        }
        let mode = parse_mode(mode_val)?;
        out.push(crate::types::ServiceCatalogueEntry { service_id, mode });
    }
    Ok(out)
}

pub fn parse_service_subscription_snapshot(
    value: &serde_json::Value,
) -> pi_js::Result<crate::types::ServiceSubscriptionSnapshot> {
    assert_subscription_snapshot(value, &crate::delta::assert_valid_op)?;
    serde_json::from_value(normalize_wire_numbers(value))
        .map_err(|e| pi_js::Error::js("TypeError", format!("Invalid service subscription snapshot: {e}")))
}

pub fn parse_wire_service_subscription_snapshot(
    value: &serde_json::Value,
) -> pi_js::Result<WireServiceSubscriptionSnapshot> {
    assert_subscription_snapshot(value, &crate::delta::assert_valid_wire_op)?;
    serde_json::from_value(normalize_wire_numbers(value))
        .map_err(|e| pi_js::Error::js("TypeError", format!("Invalid service subscription snapshot: {e}")))
}

pub fn parse_service_provider_update(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceProviderUpdate> {
    assert_provider_update(value, &crate::delta::assert_valid_op)?;
    serde_json::from_value(normalize_wire_numbers(value))
        .map_err(|e| pi_js::Error::js("TypeError", format!("Invalid service provider update: {e}")))
}

pub fn parse_wire_service_provider_update(value: &serde_json::Value) -> pi_js::Result<WireServiceProviderUpdate> {
    assert_provider_update(value, &crate::delta::assert_valid_wire_op)?;
    serde_json::from_value(normalize_wire_numbers(value))
        .map_err(|e| pi_js::Error::js("TypeError", format!("Invalid service provider update: {e}")))
}

fn json_to_i64(value: &serde_json::Value) -> Option<i64> {
    if let Some(i) = value.as_i64() {
        return Some(i);
    }
    if let Some(u) = value.as_u64() {
        return i64::try_from(u).ok();
    }
    if let Some(f) = value.as_f64() {
        if f.is_finite() && f.fract() == 0.0 && f >= (i64::MIN as f64) && f <= (i64::MAX as f64) {
            return Some(f as i64);
        }
    }
    None
}

fn parse_mode(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceMode> {
    match value.as_str() {
        Some("singleton") => Ok(crate::types::ServiceMode::Singleton),
        Some("keyed") => Ok(crate::types::ServiceMode::Keyed),
        _ => Err(pi_js::Error::js("TypeError", "Invalid service mode")),
    }
}

/// Normalizes integer-valued floats (`1.0`) in validated `sequence`/`generation` positions so `serde_json::from_value` accepts them cleanly.
fn normalize_wire_numbers(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let normalized = if (k == "sequence" || k == "generation") && v.is_number() {
                    if let Some(i) = json_to_i64(v) {
                        serde_json::Value::Number(i.into())
                    } else {
                        v.clone()
                    }
                } else if k == "ops" || matches!(k.as_str(), "instances" | "members" | "snapshot" | "instance") {
                    normalize_wire_numbers(v)
                } else {
                    v.clone()
                };
                out.insert(k.clone(), normalized);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(normalize_wire_numbers).collect()),
        _ => value.clone(),
    }
}

fn assert_subscription_snapshot(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    let snapshot = record(value, "service subscription snapshot")?;
    assert_keys(
        snapshot,
        &["serviceId", "mode", "instances"],
        &[],
        "service subscription snapshot",
    )?;
    let Some(service_id) = snapshot.get("serviceId") else {
        return Err(pi_js::Error::js("TypeError", "Invalid service subscription snapshot"));
    };
    let Some(mode) = snapshot.get("mode") else {
        return Err(pi_js::Error::js("TypeError", "Invalid service subscription snapshot"));
    };
    let Some(instances) = snapshot.get("instances").and_then(|v| v.as_array()) else {
        return Err(pi_js::Error::js("TypeError", "Invalid service subscription snapshot"));
    };
    if !is_id(service_id) || !is_mode(mode) {
        return Err(pi_js::Error::js("TypeError", "Invalid service subscription snapshot"));
    }
    for instance in instances {
        assert_instance(instance, assert_op)?;
    }
    Ok(())
}

fn assert_provider_update(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    let update = record(value, "service provider update")?;
    match update.get("type").and_then(|v| v.as_str()) {
        Some("state") => {
            assert_keys(
                update,
                &["type", "member", "sequence", "ops"],
                &["instance"],
                "state update",
            )?;
            let member = update.get("member");
            let sequence = update.get("sequence");
            let ops = update.get("ops").and_then(|v| v.as_array());
            if !member.is_some_and(is_id) || !sequence.is_some_and(|v| is_integer(v, 1)) || ops.is_none() {
                return Err(pi_js::Error::js("TypeError", "Invalid service state update"));
            }
            if let Some(instance) = update.get("instance") {
                assert_address(instance)?;
            }
            for op in ops.unwrap() {
                assert_op(op)?;
            }
            Ok(())
        }
        Some("reset") => {
            assert_keys(update, &["type", "snapshot"], &[], "reset update")?;
            let snapshot_val = update
                .get("snapshot")
                .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid reset update"))?;
            assert_subscription_snapshot(snapshot_val, assert_op)?;
            if let Some(instances) = snapshot_val.get("instances").and_then(|v| v.as_array()) {
                for instance in instances {
                    if let Some(members) = instance.get("members").and_then(|v| v.as_array()) {
                        for member in members {
                            if member.get("kind").and_then(|v| v.as_str()) == Some("state") {
                                let ops = member.get("ops").and_then(|v| v.as_array());
                                let is_root_replace = ops.is_some_and(|list| {
                                    list.len() == 1
                                        && list[0]
                                            .as_array()
                                            .and_then(|op| op.first())
                                            .and_then(|verb| verb.as_str())
                                            == Some("r")
                                });
                                if !is_root_replace {
                                    return Err(pi_js::Error::js(
                                        "TypeError",
                                        "Service reset must contain full root replacements",
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Some("unavailable") => {
            assert_keys(update, &["type"], &[], "unavailable update")?;
            Ok(())
        }
        Some("replaced") => {
            assert_keys(update, &["type", "snapshot"], &[], "replacement update")?;
            let snapshot = update
                .get("snapshot")
                .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid replacement update"))?;
            assert_instance(snapshot, assert_op)
        }
        Some("spawned") => {
            assert_keys(update, &["type", "instance"], &[], "spawn update")?;
            let instance = update
                .get("instance")
                .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid spawn update"))?;
            assert_instance(instance, assert_op)
        }
        Some("closed") => {
            assert_keys(update, &["type", "instance"], &[], "close update")?;
            let instance = update
                .get("instance")
                .ok_or_else(|| pi_js::Error::js("TypeError", "Invalid close update"))?;
            assert_address(instance)?;
            Ok(())
        }
        _ => Err(pi_js::Error::js("TypeError", "Invalid service provider update")),
    }
}

fn assert_instance(
    value: &serde_json::Value,
    assert_op: &dyn Fn(&serde_json::Value) -> pi_js::Result<()>,
) -> pi_js::Result<()> {
    let instance = record(value, "service instance snapshot")?;
    assert_keys(instance, &["members"], &["instance"], "service instance snapshot")?;
    if let Some(addr) = instance.get("instance") {
        assert_address(addr)?;
    }
    let Some(members) = instance.get("members").and_then(|v| v.as_array()) else {
        return Err(pi_js::Error::js("TypeError", "Invalid service instance snapshot"));
    };
    for candidate in members {
        let member = record(candidate, "service member snapshot")?;
        match member.get("kind").and_then(|v| v.as_str()) {
            Some("method") => {
                assert_keys(member, &["name", "kind"], &[], "service method snapshot")?;
                if !member.get("name").is_some_and(is_id) {
                    return Err(pi_js::Error::js("TypeError", "Invalid service method snapshot"));
                }
            }
            Some("state") => {
                assert_keys(
                    member,
                    &["name", "kind", "sequence", "ops"],
                    &[],
                    "service state snapshot",
                )?;
                let name = member.get("name");
                let sequence = member.get("sequence");
                let ops = member.get("ops").and_then(|v| v.as_array());
                if !name.is_some_and(is_id) || !sequence.is_some_and(|v| is_integer(v, 0)) || ops.is_none() {
                    return Err(pi_js::Error::js("TypeError", "Invalid service state snapshot"));
                }
                for op in ops.unwrap() {
                    assert_op(op)?;
                }
            }
            _ => return Err(pi_js::Error::js("TypeError", "Invalid service member snapshot")),
        }
    }
    Ok(())
}

fn assert_address(value: &serde_json::Value) -> pi_js::Result<crate::types::ServiceInstanceAddress> {
    let address = record(value, "service instance address")?;
    assert_keys(address, &["key", "generation"], &[], "service instance address")?;
    let key = address.get("key");
    let generation = address.get("generation");
    if !key.is_some_and(is_id) || !generation.is_some_and(|v| is_integer(v, 1)) {
        return Err(pi_js::Error::js("TypeError", "Invalid service instance address"));
    }
    Ok(crate::types::ServiceInstanceAddress {
        key: key.unwrap().as_str().unwrap().to_string(),
        generation: json_to_i64(generation.unwrap()).unwrap(),
    })
}

fn record<'a>(
    value: &'a serde_json::Value,
    description: &str,
) -> pi_js::Result<&'a serde_json::Map<String, serde_json::Value>> {
    value
        .as_object()
        .ok_or_else(|| pi_js::Error::js("TypeError", format!("Invalid {description}")))
}

fn assert_keys(
    value: &serde_json::Map<String, serde_json::Value>,
    required: &[&str],
    optional: &[&str],
    description: &str,
) -> pi_js::Result<()> {
    if required.iter().any(|key| !value.contains_key(*key))
        || value
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(pi_js::Error::js("TypeError", format!("Invalid {description}")));
    }
    Ok(())
}

fn is_id(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}

fn is_mode(value: &serde_json::Value) -> bool {
    matches!(value.as_str(), Some("singleton" | "keyed"))
}

fn is_integer(value: &serde_json::Value, minimum: i64) -> bool {
    json_to_i64(value).is_some_and(|n| n >= minimum)
}

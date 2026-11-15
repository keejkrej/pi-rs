//! Port of packages/chord/src/services/state-codec.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use crate::services::wire::{WireServiceInstanceSnapshot, WireServiceProviderUpdate, WireServiceSubscriptionSnapshot};

/// Stateful operation encoders for every replicated state in one service subscription.
#[derive(Clone)]
pub struct ServiceStateEncoder {
    inner: Arc<ServiceStateEncoderInner>,
}

struct ServiceStateEncoderInner {
    codecs: Mutex<StateCodecRegistry<crate::delta::Encoder>>,
}

/// Stateful operation decoders for every replicated state in one service subscription.
#[derive(Clone)]
pub struct ServiceStateDecoder {
    inner: Arc<ServiceStateDecoderInner>,
}

struct ServiceStateDecoderInner {
    codecs: Mutex<StateCodecRegistry<crate::delta::Decoder>>,
}

struct CodecEntry<C> {
    instance: Option<crate::types::ServiceInstanceAddress>,
    codec: C,
}

struct StateCodecRegistry<C> {
    create: Arc<dyn Fn() -> C + Send + Sync>,
    entries: indexmap::IndexMap<String, CodecEntry<C>>,
}

impl<C> StateCodecRegistry<C> {
    fn reset(&mut self) {
        self.entries.clear();
    }

    fn add<'a>(
        &'a mut self,
        instance: Option<&crate::types::ServiceInstanceAddress>,
        member: &str,
    ) -> pi_js::Result<&'a mut C> {
        let key = state_key(instance, member);
        if self.entries.contains_key(&key) {
            return Err(pi_js::Error::msg(format!(
                "Duplicate service state {}",
                describe_state(instance, member)
            )));
        }
        let codec = (self.create)();
        let entry = CodecEntry {
            instance: instance.cloned(),
            codec,
        };
        let index = self.entries.insert_full(key, entry).0;
        Ok(&mut self.entries.get_index_mut(index).unwrap().1.codec)
    }

    fn get<'a>(
        &'a mut self,
        instance: Option<&crate::types::ServiceInstanceAddress>,
        member: &str,
    ) -> pi_js::Result<&'a mut C> {
        let key = state_key(instance, member);
        self.entries
            .get_mut(&key)
            .map(|entry| &mut entry.codec)
            .ok_or_else(|| pi_js::Error::msg(format!("Unknown service state {}", describe_state(instance, member))))
    }

    fn remove_instance(&mut self, instance: &crate::types::ServiceInstanceAddress) {
        let keys_to_remove: Vec<String> = self
            .entries
            .iter()
            .filter_map(|(k, entry)| {
                if same_address(entry.instance.as_ref(), instance) {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for k in keys_to_remove {
            self.entries.shift_remove(&k);
        }
    }
}

impl ServiceStateEncoder {
    pub fn encode_snapshot(
        &self,
        snapshot: &crate::types::ServiceSubscriptionSnapshot,
    ) -> pi_js::Result<WireServiceSubscriptionSnapshot> {
        let mut codecs = self.inner.codecs.lock().unwrap();
        codecs.reset();
        let mut instances = Vec::with_capacity(snapshot.instances.len());
        for instance in &snapshot.instances {
            instances.push(encode_instance(instance, &mut codecs)?);
        }
        Ok(WireServiceSubscriptionSnapshot {
            service_id: snapshot.service_id.clone(),
            mode: snapshot.mode,
            instances,
        })
    }

    pub fn encode_update(
        &self,
        update: &crate::types::ServiceProviderUpdate,
    ) -> pi_js::Result<WireServiceProviderUpdate> {
        let mut codecs = self.inner.codecs.lock().unwrap();
        match update {
            crate::types::ServiceProviderUpdate::Reset { snapshot } => {
                codecs.reset();
                let mut instances = Vec::with_capacity(snapshot.instances.len());
                for instance in &snapshot.instances {
                    instances.push(encode_instance(instance, &mut codecs)?);
                }
                Ok(WireServiceProviderUpdate::Reset {
                    snapshot: WireServiceSubscriptionSnapshot {
                        service_id: snapshot.service_id.clone(),
                        mode: snapshot.mode,
                        instances,
                    },
                })
            }
            crate::types::ServiceProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let encoded_ops = codecs.get(instance.as_ref(), member)?.encode(ops);
                Ok(WireServiceProviderUpdate::State {
                    instance: instance.clone(),
                    member: member.clone(),
                    sequence: *sequence,
                    ops: encoded_ops,
                })
            }
            crate::types::ServiceProviderUpdate::Replaced { snapshot } => {
                codecs.reset();
                Ok(WireServiceProviderUpdate::Replaced {
                    snapshot: encode_instance(snapshot, &mut codecs)?,
                })
            }
            crate::types::ServiceProviderUpdate::Spawned { instance } => Ok(WireServiceProviderUpdate::Spawned {
                instance: encode_instance(instance, &mut codecs)?,
            }),
            crate::types::ServiceProviderUpdate::Unavailable => {
                codecs.reset();
                Ok(WireServiceProviderUpdate::Unavailable)
            }
            crate::types::ServiceProviderUpdate::Closed { instance } => {
                codecs.remove_instance(instance);
                Ok(WireServiceProviderUpdate::Closed {
                    instance: instance.clone(),
                })
            }
        }
    }
}

impl ServiceStateDecoder {
    pub fn decode_snapshot(
        &self,
        snapshot: &WireServiceSubscriptionSnapshot,
    ) -> pi_js::Result<crate::types::ServiceSubscriptionSnapshot> {
        let mut codecs = self.inner.codecs.lock().unwrap();
        codecs.reset();
        let mut instances = Vec::with_capacity(snapshot.instances.len());
        for instance in &snapshot.instances {
            instances.push(decode_instance(instance, &mut codecs)?);
        }
        Ok(crate::types::ServiceSubscriptionSnapshot {
            service_id: snapshot.service_id.clone(),
            mode: snapshot.mode,
            instances,
        })
    }

    pub fn decode_update(
        &self,
        update: &WireServiceProviderUpdate,
    ) -> pi_js::Result<crate::types::ServiceProviderUpdate> {
        let mut codecs = self.inner.codecs.lock().unwrap();
        match update {
            WireServiceProviderUpdate::Reset { snapshot } => {
                codecs.reset();
                let mut instances = Vec::with_capacity(snapshot.instances.len());
                for instance in &snapshot.instances {
                    instances.push(decode_instance(instance, &mut codecs)?);
                }
                Ok(crate::types::ServiceProviderUpdate::Reset {
                    snapshot: crate::types::ServiceSubscriptionSnapshot {
                        service_id: snapshot.service_id.clone(),
                        mode: snapshot.mode,
                        instances,
                    },
                })
            }
            WireServiceProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let decoded_ops = codecs.get(instance.as_ref(), member)?.decode(ops)?;
                Ok(crate::types::ServiceProviderUpdate::State {
                    instance: instance.clone(),
                    member: member.clone(),
                    sequence: *sequence,
                    ops: decoded_ops,
                })
            }
            WireServiceProviderUpdate::Replaced { snapshot } => {
                codecs.reset();
                Ok(crate::types::ServiceProviderUpdate::Replaced {
                    snapshot: decode_instance(snapshot, &mut codecs)?,
                })
            }
            WireServiceProviderUpdate::Spawned { instance } => Ok(crate::types::ServiceProviderUpdate::Spawned {
                instance: decode_instance(instance, &mut codecs)?,
            }),
            WireServiceProviderUpdate::Unavailable => {
                codecs.reset();
                Ok(crate::types::ServiceProviderUpdate::Unavailable)
            }
            WireServiceProviderUpdate::Closed { instance } => {
                codecs.remove_instance(instance);
                Ok(crate::types::ServiceProviderUpdate::Closed {
                    instance: instance.clone(),
                })
            }
        }
    }
}

pub fn create_service_state_encoder() -> ServiceStateEncoder {
    ServiceStateEncoder {
        inner: Arc::new(ServiceStateEncoderInner {
            codecs: Mutex::new(StateCodecRegistry {
                create: Arc::new(crate::delta::encoder),
                entries: indexmap::IndexMap::new(),
            }),
        }),
    }
}

pub fn create_service_state_decoder() -> ServiceStateDecoder {
    ServiceStateDecoder {
        inner: Arc::new(ServiceStateDecoderInner {
            codecs: Mutex::new(StateCodecRegistry {
                create: Arc::new(crate::delta::decoder),
                entries: indexmap::IndexMap::new(),
            }),
        }),
    }
}

fn encode_instance(
    instance: &crate::types::ServiceInstanceSnapshot,
    codecs: &mut StateCodecRegistry<crate::delta::Encoder>,
) -> pi_js::Result<WireServiceInstanceSnapshot> {
    let mut members = Vec::with_capacity(instance.members.len());
    for member in &instance.members {
        match member {
            crate::types::ServiceMemberSnapshot::Method(m) => {
                members.push(crate::services::wire::WireServiceMemberSnapshot::Method(
                    crate::services::wire::WireServiceMethodSnapshot {
                        name: m.name.clone(),
                        kind: crate::services::wire::WireServiceMethodKind::Method,
                    },
                ));
            }
            crate::types::ServiceMemberSnapshot::State(s) => {
                let encoded_ops = codecs.add(instance.instance.as_ref(), &s.name)?.encode(&s.ops);
                members.push(crate::services::wire::WireServiceMemberSnapshot::State(
                    crate::services::wire::WireServiceStateSnapshot {
                        name: s.name.clone(),
                        kind: crate::services::wire::WireServiceStateKind::State,
                        sequence: s.sequence,
                        ops: encoded_ops,
                    },
                ));
            }
        }
    }
    Ok(WireServiceInstanceSnapshot {
        instance: instance.instance.clone(),
        members,
    })
}

fn decode_instance(
    instance: &WireServiceInstanceSnapshot,
    codecs: &mut StateCodecRegistry<crate::delta::Decoder>,
) -> pi_js::Result<crate::types::ServiceInstanceSnapshot> {
    let mut members = Vec::with_capacity(instance.members.len());
    for member in &instance.members {
        match member {
            crate::services::wire::WireServiceMemberSnapshot::Method(m) => {
                members.push(crate::types::ServiceMemberSnapshot::Method(
                    crate::types::ServiceMethodSnapshot {
                        name: m.name.clone(),
                        kind: crate::types::ServiceMemberMethodKind::Method,
                    },
                ));
            }
            crate::services::wire::WireServiceMemberSnapshot::State(s) => {
                let decoded_ops = codecs.add(instance.instance.as_ref(), &s.name)?.decode(&s.ops)?;
                members.push(crate::types::ServiceMemberSnapshot::State(
                    crate::types::ServiceStateSnapshot {
                        name: s.name.clone(),
                        kind: crate::types::ServiceMemberStateKind::State,
                        sequence: s.sequence,
                        ops: decoded_ops,
                    },
                ));
            }
        }
    }
    Ok(crate::types::ServiceInstanceSnapshot {
        instance: instance.instance.clone(),
        members,
    })
}

fn state_key(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    let tuple = serde_json::json!([
        instance
            .map(|addr| serde_json::Value::String(addr.key.clone()))
            .unwrap_or(serde_json::Value::Null),
        instance
            .map(|addr| serde_json::Value::Number(addr.generation.into()))
            .unwrap_or(serde_json::Value::Null),
        member,
    ]);
    pi_js::json::stringify(&tuple)
}

fn same_address(
    left: Option<&crate::types::ServiceInstanceAddress>,
    right: &crate::types::ServiceInstanceAddress,
) -> bool {
    left.is_some_and(|addr| addr.key == right.key && addr.generation == right.generation)
}

fn describe_state(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    match instance {
        None => member.to_string(),
        Some(addr) => format!("{}@{}.{}", addr.key, addr.generation, member),
    }
}

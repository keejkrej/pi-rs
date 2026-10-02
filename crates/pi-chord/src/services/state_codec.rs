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
        todo!("port: StateCodecRegistry::reset")
    }

    fn add<'a>(
        &'a mut self,
        instance: Option<&crate::types::ServiceInstanceAddress>,
        member: &str,
    ) -> pi_js::Result<&'a mut C> {
        todo!("port: StateCodecRegistry::add")
    }

    fn get<'a>(
        &'a mut self,
        instance: Option<&crate::types::ServiceInstanceAddress>,
        member: &str,
    ) -> pi_js::Result<&'a mut C> {
        todo!("port: StateCodecRegistry::get")
    }

    fn remove_instance(&mut self, instance: &crate::types::ServiceInstanceAddress) {
        todo!("port: StateCodecRegistry::remove_instance")
    }
}

impl ServiceStateEncoder {
    pub fn encode_snapshot(
        &self,
        snapshot: &crate::types::ServiceSubscriptionSnapshot,
    ) -> pi_js::Result<WireServiceSubscriptionSnapshot> {
        todo!("port: ServiceStateEncoder::encode_snapshot")
    }

    pub fn encode_update(
        &self,
        update: &crate::types::ServiceProviderUpdate,
    ) -> pi_js::Result<WireServiceProviderUpdate> {
        todo!("port: ServiceStateEncoder::encode_update")
    }
}

impl ServiceStateDecoder {
    pub fn decode_snapshot(
        &self,
        snapshot: &WireServiceSubscriptionSnapshot,
    ) -> pi_js::Result<crate::types::ServiceSubscriptionSnapshot> {
        todo!("port: ServiceStateDecoder::decode_snapshot")
    }

    pub fn decode_update(
        &self,
        update: &WireServiceProviderUpdate,
    ) -> pi_js::Result<crate::types::ServiceProviderUpdate> {
        todo!("port: ServiceStateDecoder::decode_update")
    }
}

pub fn create_service_state_encoder() -> ServiceStateEncoder {
    todo!("port: create_service_state_encoder")
}

pub fn create_service_state_decoder() -> ServiceStateDecoder {
    todo!("port: create_service_state_decoder")
}

fn encode_instance(
    instance: &crate::types::ServiceInstanceSnapshot,
    codecs: &mut StateCodecRegistry<crate::delta::Encoder>,
) -> pi_js::Result<WireServiceInstanceSnapshot> {
    todo!("port: encode_instance")
}

fn decode_instance(
    instance: &WireServiceInstanceSnapshot,
    codecs: &mut StateCodecRegistry<crate::delta::Decoder>,
) -> pi_js::Result<crate::types::ServiceInstanceSnapshot> {
    todo!("port: decode_instance")
}

fn state_key(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    todo!("port: state_key")
}

fn same_address(
    left: Option<&crate::types::ServiceInstanceAddress>,
    right: &crate::types::ServiceInstanceAddress,
) -> bool {
    todo!("port: same_address")
}

fn describe_state(instance: Option<&crate::types::ServiceInstanceAddress>, member: &str) -> String {
    todo!("port: describe_state")
}

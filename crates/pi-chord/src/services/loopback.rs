//! Port of packages/chord/src/services/loopback.ts

#![allow(dead_code, unused_variables)]

use crate::services::provider::RemoteServiceProvider;
use crate::types::RemoteServiceTransport;

/// Connects a provider to a binding without changing remote service semantics.
pub fn create_loopback_service_transport(provider: &RemoteServiceProvider) -> RemoteServiceTransport {
    todo!("port: create_loopback_service_transport")
}

//! Port of packages/chord/src/api.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::types::{
    AttachedReplicatedState, Facet, FacetHost, FacetLoader, FacetOptions, JsonValue, MutableReplicatedState,
    RemoteServiceBinding, RemoteServiceBindingOptions, ReplicatedStateSource, ReplicatedStateSourceOptions, Service,
};

#[derive(Clone, Debug, Default)]
pub struct DefineServiceOptions {
    pub local: Option<bool>,
}

/// Create an active host for one complete set of facets.
pub async fn create_facet_host(options: FacetOptions) -> pi_js::Result<FacetHost> {
    todo!("port: create_facet_host")
}

pub fn create_static_facet_loader(facets: Vec<Facet>) -> FacetLoader {
    todo!("port: create_static_facet_loader")
}

pub fn combine_facet_loaders(loaders: Vec<FacetLoader>) -> FacetLoader {
    todo!("port: combine_facet_loaders")
}

pub fn define_facet(facet: Facet) -> Facet {
    facet
}

// TODO: check if the reserved namespace should be part of Chord.
// PORT: TS overloads collapse; `RemoteServiceContract<T>` is a type-only constraint.
pub fn define_service(id: &str, options: Option<DefineServiceOptions>) -> pi_js::Result<Service> {
    todo!("port: define_service")
}

pub fn create_remote_service_binding(options: RemoteServiceBindingOptions) -> pi_js::Result<RemoteServiceBinding> {
    todo!("port: create_remote_service_binding")
}

// PORT: TS overloads of replicatedState. `replicated_state` is the initial-value overload.
/// Create authoritative state by taking immutable ownership of an alias-free strict-JSON root.
/// The caller must not mutate `initial` after this call.
pub fn replicated_state_initial(initial: JsonValue) -> pi_js::Result<MutableReplicatedState> {
    todo!("port: replicated_state_initial")
}

pub fn replicated_state_source(
    source: Arc<dyn ReplicatedStateSource<JsonValue> + Send + Sync>,
    options: Option<ReplicatedStateSourceOptions>,
) -> pi_js::Result<AttachedReplicatedState> {
    todo!("port: replicated_state_source")
}

fn is_replicated_state_source(value: &dyn std::any::Any) -> bool {
    todo!("port: is_replicated_state_source")
}

//! Port of packages/chord/src/api.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::facets::host::FacetKernel;
use crate::facets::loader::dispose_loaded_facets;
use crate::services::errors::AggregateError;
use crate::services::state::{MutableReplicatedStateImpl, attach_replicated_state_source};
use crate::types::{
    AttachedReplicatedState, Facet, FacetHost, FacetLoader, FacetOptions, JsonValue, LoadedFacets,
    MutableReplicatedState, RemoteServiceBinding, RemoteServiceBindingOptions, ReplicatedStateSource,
    ReplicatedStateSourceAttachment, ReplicatedStateSourceOptions, Service,
};

#[derive(Clone, Debug, Default)]
pub struct DefineServiceOptions {
    pub local: Option<bool>,
}

/// Create an active host for one complete set of facets.
pub async fn create_facet_host(options: FacetOptions) -> pi_js::Result<FacetHost> {
    let kernel = FacetKernel::new(options)?;
    kernel.activate().await?;
    Ok(FacetHost::from_kernel(kernel))
}

pub fn create_static_facet_loader(facets: Vec<Facet>) -> FacetLoader {
    FacetLoader::new(Arc::new(move || {
        let facets = facets.clone();
        Box::pin(async move {
            Ok(LoadedFacets::new(
                facets,
                Arc::new(|| Box::pin(futures::future::ready(Ok(())))),
            ))
        })
    }))
}

pub fn combine_facet_loaders(loaders: Vec<FacetLoader>) -> FacetLoader {
    FacetLoader::new(Arc::new(move || {
        let loaders = loaders.clone();
        Box::pin(async move {
            let mut loaded = Vec::new();
            for loader in &loaders {
                match loader.load().await {
                    Ok(batch) => loaded.push(batch),
                    Err(error) => {
                        let cleanup_errors = dispose_loaded_facets(loaded.into_iter().rev().collect()).await;
                        if !cleanup_errors.is_empty() {
                            let mut all = vec![error];
                            all.extend(cleanup_errors);
                            return Err(pi_js::Error::typed(
                                "AggregateError",
                                AggregateError {
                                    message: "Facet loading and cleanup failed".to_string(),
                                    errors: all,
                                },
                            ));
                        }
                        return Err(error);
                    }
                }
            }

            let combined_facets: Vec<Facet> = loaded.iter().flat_map(|item| item.facets().iter().cloned()).collect();
            let disposed = Arc::new(AtomicBool::new(false));
            let loaded_arc = Arc::new(loaded);
            let disposal = Arc::new(move || {
                let disposed = Arc::clone(&disposed);
                let loaded_arc = Arc::clone(&loaded_arc);
                Box::pin(async move {
                    if disposed.swap(true, Ordering::AcqRel) {
                        return Ok(());
                    }
                    let reversed: Vec<LoadedFacets> = loaded_arc.iter().rev().cloned().collect();
                    let errors = dispose_loaded_facets(reversed).await;
                    crate::services::errors::throw_collected_errors(&errors, "Failed to dispose loaded facets")
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            });

            Ok(LoadedFacets::new(combined_facets, disposal))
        })
    }))
}

pub fn define_facet(facet: Facet) -> Facet {
    facet
}

// TODO: check if the reserved namespace should be part of Chord.
// PORT: TS overloads collapse; `RemoteServiceContract<T>` is a type-only constraint.
pub fn define_service(id: &str, options: Option<DefineServiceOptions>) -> pi_js::Result<Service> {
    if id.is_empty() {
        return Err(pi_js::Error::js("TypeError", "Service ID must not be empty"));
    }
    if id.starts_with("$chord.") {
        return Err(pi_js::Error::js(
            "TypeError",
            "Service IDs beginning with $chord. are reserved",
        ));
    }
    Ok(Service::new(id, options.and_then(|o| o.local).unwrap_or(false)))
}

pub fn create_remote_service_binding(options: RemoteServiceBindingOptions) -> pi_js::Result<RemoteServiceBinding> {
    RemoteServiceBinding::new(options)
}

// PORT: TS overloads of replicatedState. `replicated_state` is the initial-value overload.
/// Create authoritative state by taking immutable ownership of an alias-free strict-JSON root.
/// The caller must not mutate `initial` after this call.
pub fn replicated_state_initial(initial: JsonValue) -> pi_js::Result<MutableReplicatedState> {
    let impl_ = MutableReplicatedStateImpl::new(initial)?;
    Ok(MutableReplicatedState::from_impl(impl_))
}

struct ArcReplicatedStateSource<T>(Arc<dyn ReplicatedStateSource<T> + Send + Sync>);

impl<T: Send + Sync> ReplicatedStateSource<T> for ArcReplicatedStateSource<T> {
    fn attach(&self) -> pi_js::Result<Box<dyn ReplicatedStateSourceAttachment<T> + Send + Sync>> {
        self.0.attach()
    }
}

pub fn replicated_state_source(
    source: Arc<dyn ReplicatedStateSource<JsonValue> + Send + Sync>,
    options: Option<ReplicatedStateSourceOptions>,
) -> pi_js::Result<AttachedReplicatedState> {
    let impl_ = attach_replicated_state_source(ArcReplicatedStateSource(source), options)?;
    Ok(AttachedReplicatedState::from_impl(impl_))
}

//! Port of packages/chord/src/index.ts
//!
//! Chord is a standalone application-composition runtime for agentic applications.

// @generated-mods begin (scaffold-owned, do not edit)
pub mod api;
pub mod bundler;
pub mod context;
pub mod delta;
pub mod facets;
pub mod json;
pub mod node;
pub mod services;
pub mod types;
// @generated-mods end
pub use pi_js::{Error, Result};

pub use crate::api::{
    combine_facet_loaders, create_facet_host, create_remote_service_binding, create_static_facet_loader, define_facet,
    define_service, replicated_state_initial as replicated_state, replicated_state_source,
};
pub use crate::delta::Draft;
pub use crate::json::{CopyJsonOptions, copy_json, is_json_value};
pub use crate::services::errors::{
    REMOTE_SERVICE_ERROR_CODES, RemoteServiceError, RemoteServiceErrorCode, is_remote_service_error_code,
};
pub use crate::services::provider::{
    RemoteServiceEndpoint, RemoteServiceProvider, ServiceUpdatePublisher, create_remote_service_endpoint,
};
pub use crate::services::state_codec::{
    ServiceStateDecoder, ServiceStateEncoder, create_service_state_decoder, create_service_state_encoder,
};
pub use crate::services::wire::{
    ServiceControlCall, WireServiceInstanceSnapshot, WireServiceMemberSnapshot, WireServiceProviderUpdate,
    WireServiceSubscriptionSnapshot, create_service_catalogue_call, create_service_subscribe_call,
    create_service_unsubscribe_call, decode_service_control_call, parse_service_call, parse_service_catalogue,
    parse_service_provider_update, parse_service_subscription_snapshot, parse_wire_service_provider_update,
    parse_wire_service_subscription_snapshot,
};
pub use crate::types::{
    AttachedReplicatedState, Context, ContextKey, Facet, FacetEnvironment, FacetHost, FacetLoader, FacetOptions,
    JsonRepresentation, JsonValue, LoadedFacets, MutableReplicatedState, RemoteServiceBinding,
    RemoteServiceBindingOptions, RemoteServiceSource, RemoteServiceTransport, RemoteServices, ReplicatedState,
    ReplicatedStateDelivery, ReplicatedStateSource, ReplicatedStateSourceAttachment, ReplicatedStateSourceFrame,
    ReplicatedStateSourceOptions, Service, ServiceCall, ServiceCatalogueEntry, ServiceInstanceAddress,
    ServiceInstanceSnapshot, ServiceMemberSnapshot, ServiceMode, ServiceProviderUpdate, ServiceSpawner,
    ServiceSubscription, ServiceSubscriptionSnapshot,
};

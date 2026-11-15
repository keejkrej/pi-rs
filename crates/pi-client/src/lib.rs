//! Port of packages/client/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod client;
pub mod connection;
pub mod errors;
pub mod promise;
pub mod transport;
pub mod types;
pub mod unix;
// @generated-mods end

pub use pi_js::{Error, Result};

pub use crate::client::{Client, ServiceResult, create_client_service_transport};
pub use crate::errors::{ClientDisposedError, DisconnectedError, ServerError};
pub use crate::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
pub use crate::types::{
    AttachmentChangeListener, ClientOptions, ConnectionState, ConnectionStateChange, ConnectionStateListener,
    ListenerErrorHandler, ServiceSubscription, ServiceUpdateListener, Unsubscribe,
};

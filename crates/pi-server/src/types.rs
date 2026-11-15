//! Port of packages/server/src/types.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_chord::{Context, JsonValue, ServiceCall, ServiceUpdatePublisher};
use pi_js::{BoxFuture, Result};

use crate::listener::ServerListener;

pub type ConnectionCountCallback = Arc<dyn Fn(usize) + Send + Sync>;
pub type ServerErrorCallback = Arc<dyn Fn(&pi_js::Error) + Send + Sync>;

#[derive(Default)]
pub struct ServerOptions {
    pub listeners: Vec<Arc<dyn ServerListener>>,
    /// Stable logical server identity supplied by the installation or profile.
    pub server_id: String,
    pub max_frame_length: Option<i64>,
    pub handshake_timeout_ms: Option<i64>,
    pub on_connection_count_changed: Option<ConnectionCountCallback>,
    pub on_error: Option<ServerErrorCallback>,
}

/// Minimal durable Session metadata the server needs for routing. Applications may extend it.
pub trait SessionMetadata: Send + Sync + 'static {
    fn id(&self) -> &str;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultSessionMetadata {
    pub id: String,
}

impl DefaultSessionMetadata {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

impl SessionMetadata for DefaultSessionMetadata {
    fn id(&self) -> &str {
        &self.id
    }
}

/// One presentation connection's live capability for a hosted Session.
#[async_trait]
pub trait RoutedSessionAttachment: Send + Sync {
    /// Route one contract-agnostic service operation to the attached Session endpoint.
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> Result<Option<JsonValue>>;

    async fn release(&self, context: Context) -> Result<()>;
}

/// Presentation-scoped routing capabilities available to server service implementations.
#[async_trait]
pub trait RoutedServerPresentation: Send + Sync {
    async fn attach_session(&self, session_id: &str, context: Context) -> Result<()>;
    async fn detach_session(&self, context: Context) -> Result<()>;
    /// Release routed attachments and handles before the application deletes durable metadata.
    async fn prepare_session_removal(&self, session_id: &str, context: Context) -> Result<()>;
}

/// One connection's server-scoped service endpoint.
#[async_trait]
pub trait RoutedServerServiceAttachment: Send + Sync {
    async fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: ServiceUpdatePublisher,
        context: Context,
    ) -> Result<Option<JsonValue>>;

    async fn release(&self, context: Context) -> Result<()>;
}

#[async_trait]
pub trait RoutedServerServiceHost: Send + Sync {
    async fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        context: Context,
    ) -> Result<Arc<dyn RoutedServerServiceAttachment>>;
}

/// A process-safe handle that acquires presentation-scoped Session capabilities.
#[async_trait]
pub trait RoutedSessionHandle: Send + Sync {
    async fn attach_client(&self, context: Context) -> Result<Arc<dyn RoutedSessionAttachment>>;

    /// Resolves with an error for unexpected termination, or `None` after an expected close.
    fn terminated(&self) -> Option<BoxFuture<Option<pi_js::Error>>> {
        None
    }

    async fn close(&self, context: Context) -> Result<()>;
}

/// Application capabilities used by server-wide management and Session routing.
#[async_trait]
pub trait ServerHost<TMetadata: SessionMetadata = DefaultSessionMetadata>: Send + Sync {
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost>;
    /// Resolve one durable Session ID or throw a bounded routing error.
    async fn resolve_session(&self, session_id: &str, context: Context) -> Result<TMetadata>;
    async fn open_session(&self, metadata: TMetadata, context: Context) -> Result<Arc<dyn RoutedSessionHandle>>;
}

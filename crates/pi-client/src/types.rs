//! Port of packages/client/src/types.ts

use std::sync::Arc;

use pi_chord::{ServiceProviderUpdate, ServiceSubscriptionSnapshot};
use pi_protocol::{RpcTarget, SessionTarget};
use serde::{Deserialize, Serialize};

use crate::transport::ByteTransportFactory;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

impl ConnectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionStateChange {
    pub state: ConnectionState,
    pub error: Option<Arc<pi_js::Error>>,
}

pub type Unsubscribe = pi_js::Unsubscribe;
pub type ListenerErrorHandler = Arc<dyn Fn(pi_js::Error) + Send + Sync>;
pub type AttachmentChangeListener = Arc<dyn Fn(Option<SessionTarget>) -> pi_js::Result<()> + Send + Sync>;
pub type ConnectionStateListener = Arc<dyn Fn(ConnectionStateChange) -> pi_js::Result<()> + Send + Sync>;
pub type ServiceUpdateListener =
    Arc<dyn Fn(ServiceProviderUpdate) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

struct ServiceSubscriptionInner {
    id: String,
    target: RpcTarget,
    snapshot: ServiceSubscriptionSnapshot,
    start: Arc<dyn Fn() + Send + Sync>,
    dispose: Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
}

/// Handle to an active subscribed Chord service over a client connection.
#[derive(Clone)]
pub struct ServiceSubscription {
    inner: Arc<ServiceSubscriptionInner>,
}

impl ServiceSubscription {
    pub fn new(
        id: impl Into<String>,
        target: RpcTarget,
        snapshot: ServiceSubscriptionSnapshot,
        start: Arc<dyn Fn() + Send + Sync>,
        dispose: Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(ServiceSubscriptionInner {
                id: id.into(),
                target,
                snapshot,
                start,
                dispose,
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn target(&self) -> &RpcTarget {
        &self.inner.target
    }

    pub fn snapshot(&self) -> &ServiceSubscriptionSnapshot {
        &self.inner.snapshot
    }

    /// Begin ordered update delivery after the caller has installed the snapshot.
    pub fn start(&self) {
        (self.inner.start)();
    }

    pub async fn dispose(&self) -> pi_js::Result<()> {
        (self.inner.dispose)().await
    }
}

#[derive(Clone)]
pub struct ClientOptions {
    pub transport_factory: ByteTransportFactory,
    /// Logical server identity expected at the physical endpoint.
    pub server_id: String,
    pub max_frame_length: Option<i64>,
    /// Reports subscriber failures without allowing them to corrupt client state.
    pub on_listener_error: Option<ListenerErrorHandler>,
}

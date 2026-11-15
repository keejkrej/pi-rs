//! Port of packages/server/src/listener.ts

use async_trait::async_trait;
use pi_js::Result;

use crate::connection::ByteConnectionAcceptor;

/// Supplies established byte connections after any required transport authentication.
#[async_trait]
pub trait ServerListener: Send + Sync {
    /// Starts listening and passes authorized connections to `accept`.
    async fn start(&self, accept: ByteConnectionAcceptor) -> Result<()>;
    async fn close(&self) -> Result<()>;
}

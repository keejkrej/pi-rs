//! Port of packages/client/src/promise.ts

use std::future::Future;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::errors::DisconnectedError;

/// One-shot settlement handle analogous to `Promise.withResolvers()`.
pub struct PromiseResolvers<T> {
    sender: Mutex<Option<oneshot::Sender<pi_js::Result<T>>>>,
}

impl<T: Send + 'static> PromiseResolvers<T> {
    /// Creates a `(resolver, future)` pair. Only the first call to `resolve` or `reject` settles the future.
    pub fn new() -> (Self, impl Future<Output = pi_js::Result<T>> + Send + 'static) {
        let (tx, rx) = oneshot::channel();
        let resolvers = Self {
            sender: Mutex::new(Some(tx)),
        };
        let future = async move {
            match rx.await {
                Ok(result) => result,
                Err(_) => Err(DisconnectedError::disconnected().into_pi_error()),
            }
        };
        (resolvers, future)
    }

    pub fn resolve(&self, value: T) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(tx) = sender {
            let _ = tx.send(Ok(value));
        }
    }

    pub fn reject(&self, error: pi_js::Error) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(tx) = sender {
            let _ = tx.send(Err(error));
        }
    }
}

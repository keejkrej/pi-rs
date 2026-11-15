//! Port of packages/chord/src/services/loopback.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::services::provider::RemoteServiceProvider;
use crate::types::{RemoteServiceTransport, ServiceSubscription};

/// Connects a provider to a binding without changing remote service semantics.
pub fn create_loopback_service_transport(provider: &RemoteServiceProvider) -> RemoteServiceTransport {
    let invoke_provider = provider.clone();
    let subscribe_provider = provider.clone();
    RemoteServiceTransport::new(
        Arc::new(move |call, context| {
            let provider = invoke_provider.clone();
            let call = call.clone();
            let context = context.clone();
            Box::pin(async move { provider.invoke(&call, context).await })
        }),
        Arc::new(move |service_id, mode, listener, _context| {
            let provider = subscribe_provider.clone();
            let service_id = service_id.to_string();
            Box::pin(async move {
                let subscription = provider.subscribe(&service_id, mode, move |update, ctx| listener(update, ctx))?;
                let sub_activate = subscription.clone();
                let sub_close = subscription.clone();
                Ok(ServiceSubscription::new(
                    subscription.snapshot().clone(),
                    Arc::new(move || sub_activate.activate()),
                    Arc::new(move |context| {
                        let sub = sub_close.clone();
                        Box::pin(async move { sub.close(context) })
                    }),
                ))
            })
        }),
    )
}

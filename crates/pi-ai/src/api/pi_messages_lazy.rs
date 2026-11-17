//! Port of packages/ai/src/api/pi-messages.lazy.ts

use std::sync::Arc;

use pi_js::BoxFuture;

use crate::api::lazy::lazy_api;
use crate::types::ProviderStreams;

pub fn pi_messages_api() -> ProviderStreams {
    let load = Arc::new(|| {
        Box::pin(async { Ok(crate::api::pi_messages::provider_streams()) }) as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

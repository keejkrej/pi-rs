//! Port of packages/ai/src/api/anthropic-messages.lazy.ts

use std::sync::Arc;

use pi_js::BoxFuture;

use crate::api::lazy::lazy_api;
use crate::types::ProviderStreams;

pub fn anthropic_messages_api() -> ProviderStreams {
    let load = Arc::new(|| {
        Box::pin(async { Ok(crate::api::anthropic_messages::provider_streams()) })
            as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

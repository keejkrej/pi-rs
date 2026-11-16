//! Port of packages/ai/src/api/mistral-conversations.lazy.ts

use std::sync::Arc;

use pi_js::BoxFuture;

use crate::api::lazy::lazy_api;
use crate::types::ProviderStreams;

pub fn mistral_conversations_api() -> ProviderStreams {
    let load = Arc::new(|| {
        Box::pin(async { Ok(crate::api::mistral_conversations::provider_streams()) })
            as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

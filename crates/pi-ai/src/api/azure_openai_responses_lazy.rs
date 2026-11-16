//! Port of packages/ai/src/api/azure-openai-responses.lazy.ts

use std::sync::Arc;

use crate::api::lazy::{LazyApiCapabilities, LoadProviderStreams, lazy_api};
use crate::types::ProviderStreams;

pub fn azure_openai_responses_api() -> ProviderStreams {
    let loader: LoadProviderStreams =
        Arc::new(|| Box::pin(async { Ok(crate::api::azure_openai_responses::provider_streams()) }));
    lazy_api(loader, Some(LazyApiCapabilities::default()))
}

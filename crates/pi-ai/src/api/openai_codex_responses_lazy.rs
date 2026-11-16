//! Port of packages/ai/src/api/openai-codex-responses.lazy.ts

use std::sync::Arc;

use crate::api::lazy::{LazyApiCapabilities, LoadProviderStreams, lazy_api};
use crate::types::ProviderStreams;

pub fn openai_codex_responses_api() -> ProviderStreams {
    let loader: LoadProviderStreams =
        Arc::new(|| Box::pin(async { Ok(crate::api::openai_codex_responses::provider_streams()) }));
    lazy_api(loader, Some(LazyApiCapabilities::default()))
}

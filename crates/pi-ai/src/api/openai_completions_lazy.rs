//! Port of packages/ai/src/api/openai-completions.lazy.ts

use std::sync::Arc;

use crate::api::lazy::{LazyApiCapabilities, LoadProviderStreams, lazy_api};
use crate::types::ProviderStreams;

pub fn openai_completions_api() -> ProviderStreams {
    let loader: LoadProviderStreams =
        Arc::new(|| Box::pin(async { Ok(crate::api::openai_completions::provider_streams()) }));
    lazy_api(loader, Some(LazyApiCapabilities::default()))
}

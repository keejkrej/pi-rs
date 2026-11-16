//! Port of packages/ai/src/api/bedrock-converse-stream.lazy.ts

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use pi_js::BoxFuture;

use crate::api::lazy::lazy_api;
use crate::types::ProviderStreams;

static BEDROCK_MODULE_OVERRIDE: LazyLock<Mutex<Option<ProviderStreams>>> = LazyLock::new(|| Mutex::new(None));

/// Overrides the dynamically loaded Bedrock implementation. Used when a build
/// registers a statically bound provider module instead of lazy lookup.
pub fn set_bedrock_provider_module(module: ProviderStreams) {
    let mut slot = BEDROCK_MODULE_OVERRIDE.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(module);
}

pub fn bedrock_converse_stream_api() -> ProviderStreams {
    let load = Arc::new(|| {
        Box::pin(async {
            let override_module = BEDROCK_MODULE_OVERRIDE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Ok(override_module.unwrap_or_else(crate::bedrock_provider::bedrock_provider_module))
        }) as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

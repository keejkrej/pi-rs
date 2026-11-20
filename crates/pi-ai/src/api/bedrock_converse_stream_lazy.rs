//! Port of packages/ai/src/api/bedrock-converse-stream.lazy.ts

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use pi_js::BoxFuture;

use crate::api::lazy::{LoadProviderStreams, lazy_api};
use crate::types::ProviderStreams;

static BEDROCK_MODULE_LOADER: LazyLock<Mutex<Option<LoadProviderStreams>>> = LazyLock::new(|| Mutex::new(None));

/// Overrides the dynamically loaded Bedrock implementation. Used when a build
/// registers a statically bound provider module instead of lazy lookup.
pub fn set_bedrock_provider_module(module: ProviderStreams) {
    let mut slot = BEDROCK_MODULE_LOADER.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(Arc::new(move || {
        let module = module.clone();
        Box::pin(async move { Ok(module) })
    }));
}

/// Overrides how the Bedrock implementation is loaded on first request.
pub fn set_bedrock_provider_loader(load: LoadProviderStreams) {
    let mut slot = BEDROCK_MODULE_LOADER.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = Some(load);
}

/// Clears any registered custom Bedrock loader/module override.
pub fn clear_bedrock_provider_loader() {
    let mut slot = BEDROCK_MODULE_LOADER.lock().unwrap_or_else(PoisonError::into_inner);
    *slot = None;
}

pub fn bedrock_converse_stream_api() -> ProviderStreams {
    let load: LoadProviderStreams = Arc::new(|| {
        Box::pin(async {
            let loader = BEDROCK_MODULE_LOADER
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(loader) = loader {
                loader().await
            } else {
                Ok(crate::bedrock_provider::bedrock_provider_module())
            }
        }) as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

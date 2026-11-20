//! Port of packages/ai/src/lazy-bedrock-provider.ts

use crate::api::bedrock_converse_stream_lazy::{clear_bedrock_provider_loader, set_bedrock_provider_loader};
use crate::bedrock_provider::bedrock_provider_module;
use pi_js::BoxFuture;
use std::sync::Arc;

/// Registers the default Bedrock provider loader.
///
/// PORT: Static Rust linkage always includes `crate::api::bedrock_converse_stream`, so this binds
/// `bedrock_provider_module()` into `bedrock_converse_stream_lazy`.
pub fn register_lazy_bedrock_provider() {
    set_bedrock_provider_loader(Arc::new(|| {
        Box::pin(async { Ok(bedrock_provider_module()) }) as BoxFuture<pi_js::Result<crate::types::ProviderStreams>>
    }));
}

/// Clears lazy Bedrock registration state.
pub fn clear_lazy_bedrock_provider() {
    clear_bedrock_provider_loader();
}

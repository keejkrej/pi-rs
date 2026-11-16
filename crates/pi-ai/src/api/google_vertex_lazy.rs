//! Port of packages/ai/src/api/google-vertex.lazy.ts

use std::sync::Arc;

use pi_js::BoxFuture;

use crate::api::lazy::lazy_api;
use crate::types::ProviderStreams;

pub fn google_vertex_api() -> ProviderStreams {
    let load = Arc::new(|| {
        Box::pin(async { Ok(crate::api::google_vertex::provider_streams()) })
            as BoxFuture<pi_js::Result<ProviderStreams>>
    });
    lazy_api(load, None)
}

//! Port of packages/ai/src/bedrock-provider.ts

use crate::types::ProviderStreams;

pub fn bedrock_provider_module() -> ProviderStreams {
    crate::api::bedrock_converse_stream::provider_streams()
}

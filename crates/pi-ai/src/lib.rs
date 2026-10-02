//! Port of packages/ai/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod api;
pub mod auth;
pub mod bedrock_provider;
pub mod bun_oauth;
pub mod cli;
pub mod compat;
pub mod env_api_keys;
pub mod image_models;
pub mod images;
pub mod images_api_registry;
pub mod legacy_api_aliases;
pub mod model_catalog;
pub mod models;
pub mod models_generated;
pub mod models_store;
pub mod oauth;
pub mod providers;
pub mod session_resources;
pub mod types;
pub mod utils;
pub mod vendor;
// @generated-mods end
pub use pi_js::vendor::typebox as schema;
pub use pi_js::{Error, Result};

// PORT: `Static<T>` is a TypeScript inference helper. It has no runtime value and no Rust type.
pub use pi_js::vendor::typebox::Schema as TSchema;
pub use pi_js::vendor::typebox::t as Type;

pub use crate::api::anthropic_messages::{AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay};
pub use crate::api::azure_openai_responses::AzureOpenAIResponsesOptions;
pub use crate::api::bedrock_converse_stream::{BedrockOptions, BedrockThinkingDisplay};
pub use crate::api::google_generative_ai::GoogleOptions;
pub use crate::api::google_shared::{GoogleApiThinkingLevel, ResolvedGoogleThinkingLevel};
pub use crate::api::google_vertex::GoogleVertexOptions;
pub use crate::api::lazy::*;
pub use crate::api::mistral_conversations::MistralOptions;
pub use crate::api::openai_codex_responses::{OpenAICodexResponsesOptions, OpenAICodexWebSocketDebugStats};
pub use crate::api::openai_completions::OpenAICompletionsOptions;
pub use crate::api::openai_responses::OpenAIResponsesOptions;
pub use crate::api::pi_messages::{PiMessagesEvent, PiMessagesOptions, PiMessagesRewriteImpact};
pub use crate::auth::context::*;
pub use crate::auth::credential_store::*;
pub use crate::auth::helpers::*;
pub use crate::auth::types::*;
pub use crate::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption, OAuthSelectPrompt,
};
pub use crate::models::*;
pub use crate::models_store::*;
pub use crate::providers::faux::*;
pub use crate::session_resources::*;
// PORT: `types` re-exports `TranscriptContext` and `AssistantMessageEventStream`, which `transcript` and
// `event_stream` also export. Same items as the TS `export *` pair.
#[allow(ambiguous_glob_reexports)]
pub use crate::types::*;
#[allow(ambiguous_glob_reexports)]
pub use crate::utils::assistant_message_frame::*;
pub use crate::utils::diagnostics::*;
#[allow(ambiguous_glob_reexports)]
pub use crate::utils::event_stream::*;
pub use crate::utils::json_parse::*;
pub use crate::utils::overflow::*;
pub use crate::utils::retry::*;
pub use crate::utils::text::{content_text, get_system_message_text, render_system_message_update};
#[allow(ambiguous_glob_reexports)]
pub use crate::utils::transcript::*;
pub use crate::utils::typebox_helpers::*;
pub use crate::utils::uuid::uuidv7;
pub use crate::utils::validation::*;

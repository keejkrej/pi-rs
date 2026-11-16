//! Port of packages/ai/src/providers/vercel-ai-gateway.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/vercel-ai-gateway.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("vercel-ai-gateway", RAW_JSON))
}

pub fn vercel_ai_gateway_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn vercel_ai_gateway_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn vercel_ai_gateway_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

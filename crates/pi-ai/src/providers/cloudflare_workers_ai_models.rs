//! Port of packages/ai/src/providers/cloudflare-workers-ai.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/cloudflare-workers-ai.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("cloudflare-workers-ai", RAW_JSON))
}

pub fn cloudflare_workers_ai_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn cloudflare_workers_ai_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn cloudflare_workers_ai_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

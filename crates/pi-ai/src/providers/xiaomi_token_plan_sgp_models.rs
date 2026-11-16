//! Port of packages/ai/src/providers/xiaomi-token-plan-sgp.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/xiaomi-token-plan-sgp.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("xiaomi-token-plan-sgp", RAW_JSON))
}

pub fn xiaomi_token_plan_sgp_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn xiaomi_token_plan_sgp_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn xiaomi_token_plan_sgp_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

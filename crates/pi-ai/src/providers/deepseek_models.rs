//! Port of packages/ai/src/providers/deepseek.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/deepseek.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("deepseek", RAW_JSON))
}

pub fn deepseek_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn deepseek_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn deepseek_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

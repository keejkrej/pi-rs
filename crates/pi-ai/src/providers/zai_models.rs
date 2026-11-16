//! Port of packages/ai/src/providers/zai.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/zai.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("zai", RAW_JSON))
}

pub fn zai_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn zai_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn zai_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

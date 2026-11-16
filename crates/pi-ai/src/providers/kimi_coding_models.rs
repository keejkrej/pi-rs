//! Port of packages/ai/src/providers/kimi-coding.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/kimi-coding.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("kimi-coding", RAW_JSON))
}

pub fn kimi_coding_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn kimi_coding_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn kimi_coding_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

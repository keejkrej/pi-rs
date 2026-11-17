//! Port of packages/ai/src/providers/huggingface.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/huggingface.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("huggingface", RAW_JSON))
}

pub fn huggingface_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn huggingface_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn huggingface_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

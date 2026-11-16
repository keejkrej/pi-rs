//! Port of packages/ai/src/providers/nvidia.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/nvidia.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("nvidia", RAW_JSON))
}

pub fn nvidia_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn nvidia_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn nvidia_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

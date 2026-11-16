//! Port of packages/ai/src/providers/fireworks.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/fireworks.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("fireworks", RAW_JSON))
}

pub fn fireworks_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn fireworks_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn fireworks_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

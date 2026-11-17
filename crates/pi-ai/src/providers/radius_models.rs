//! Port of packages/ai/src/providers/radius.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/radius.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("radius", RAW_JSON))
}

pub fn radius_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn radius_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn radius_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

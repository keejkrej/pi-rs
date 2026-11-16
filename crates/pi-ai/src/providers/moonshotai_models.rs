//! Port of packages/ai/src/providers/moonshotai.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/moonshotai.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("moonshotai", RAW_JSON))
}

pub fn moonshotai_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn moonshotai_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn moonshotai_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

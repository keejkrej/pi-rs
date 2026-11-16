//! Port of packages/ai/src/providers/ant-ling.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/ant-ling.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("ant-ling", RAW_JSON))
}

pub fn ant_ling_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn ant_ling_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn ant_ling_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

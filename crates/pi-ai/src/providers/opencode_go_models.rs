//! Port of packages/ai/src/providers/opencode-go.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/opencode-go.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("opencode-go", RAW_JSON))
}

pub fn opencode_go_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn opencode_go_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn opencode_go_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

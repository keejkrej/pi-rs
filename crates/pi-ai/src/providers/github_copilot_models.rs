//! Port of packages/ai/src/providers/github-copilot.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/github-copilot.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("github-copilot", RAW_JSON))
}

pub fn github_copilot_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn github_copilot_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn github_copilot_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

//! Port of packages/ai/src/providers/minimax-cn.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/minimax-cn.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("minimax-cn", RAW_JSON))
}

pub fn minimax_cn_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn minimax_cn_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn minimax_cn_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

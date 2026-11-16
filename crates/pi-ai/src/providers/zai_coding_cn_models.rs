//! Port of packages/ai/src/providers/zai-coding-cn.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog};
use crate::providers::all::{ProviderModelCatalog, load_provider_catalog};

const RAW_JSON: &str = include_str!("data/zai-coding-cn.json");

fn catalog() -> &'static ProviderModelCatalog {
    static CATALOG: OnceLock<ProviderModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| load_provider_catalog("zai-coding-cn", RAW_JSON))
}

pub fn zai_coding_cn_models() -> &'static ChatModelCatalog {
    &catalog().chat
}

pub fn zai_coding_cn_image_models() -> &'static ImageModelCatalog {
    &catalog().image
}

pub fn zai_coding_cn_classifier_models() -> &'static ClassifierModelCatalog {
    &catalog().classifier
}

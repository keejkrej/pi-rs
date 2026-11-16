//! Port of packages/ai/src/providers/mistral.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};

const RAW_MISTRAL_MODELS_JSON: &str = include_str!("data/mistral.json");

pub fn mistral_models() -> &'static ChatModelCatalog {
    static CATALOG: OnceLock<ChatModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_chat_catalog_json("mistral", RAW_MISTRAL_MODELS_JSON).unwrap_or_default())
}

pub fn mistral_image_models() -> &'static ImageModelCatalog {
    static CATALOG: OnceLock<ImageModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_image_catalog_json("mistral", RAW_MISTRAL_MODELS_JSON).unwrap_or_default())
}

pub fn mistral_classifier_models() -> &'static ClassifierModelCatalog {
    static CATALOG: OnceLock<ClassifierModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_classifier_catalog_json("mistral", RAW_MISTRAL_MODELS_JSON).unwrap_or_default())
}

//! Port of packages/ai/src/providers/google.models.ts

use std::sync::OnceLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};

const RAW_GOOGLE_MODELS_JSON: &str = include_str!("data/google.json");

pub fn google_models() -> &'static ChatModelCatalog {
    static CATALOG: OnceLock<ChatModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_chat_catalog_json("google", RAW_GOOGLE_MODELS_JSON).unwrap_or_default())
}

pub fn google_image_models() -> &'static ImageModelCatalog {
    static CATALOG: OnceLock<ImageModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_image_catalog_json("google", RAW_GOOGLE_MODELS_JSON).unwrap_or_default())
}

pub fn google_classifier_models() -> &'static ClassifierModelCatalog {
    static CATALOG: OnceLock<ClassifierModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| flatten_classifier_catalog_json("google", RAW_GOOGLE_MODELS_JSON).unwrap_or_default())
}

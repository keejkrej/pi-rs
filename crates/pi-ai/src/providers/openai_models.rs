//! Port of packages/ai/src/providers/openai.models.ts

use std::sync::LazyLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};
use crate::types::AnyModel;

static RAW_CATALOG_JSON: &str = include_str!("data/openai.json");

pub static OPENAI_MODELS: LazyLock<ChatModelCatalog> =
    LazyLock::new(|| flatten_chat_catalog_json("openai", RAW_CATALOG_JSON).unwrap_or_default());

pub static OPENAI_IMAGE_MODELS: LazyLock<ImageModelCatalog> =
    LazyLock::new(|| flatten_image_catalog_json("openai", RAW_CATALOG_JSON).unwrap_or_default());

pub static OPENAI_CLASSIFIER_MODELS: LazyLock<ClassifierModelCatalog> =
    LazyLock::new(|| flatten_classifier_catalog_json("openai", RAW_CATALOG_JSON).unwrap_or_default());

pub fn openai_models() -> Vec<AnyModel> {
    OPENAI_MODELS.values().cloned().map(AnyModel::Chat).collect()
}

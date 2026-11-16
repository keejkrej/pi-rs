//! Port of packages/ai/src/providers/anthropic.models.ts

use std::sync::LazyLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};
use crate::types::AnyModel;

static RAW_CATALOG_JSON: &str = include_str!("data/anthropic.json");

pub static ANTHROPIC_MODELS: LazyLock<ChatModelCatalog> =
    LazyLock::new(|| flatten_chat_catalog_json("anthropic", RAW_CATALOG_JSON).unwrap_or_default());

pub static ANTHROPIC_IMAGE_MODELS: LazyLock<ImageModelCatalog> =
    LazyLock::new(|| flatten_image_catalog_json("anthropic", RAW_CATALOG_JSON).unwrap_or_default());

pub static ANTHROPIC_CLASSIFIER_MODELS: LazyLock<ClassifierModelCatalog> =
    LazyLock::new(|| flatten_classifier_catalog_json("anthropic", RAW_CATALOG_JSON).unwrap_or_default());

pub fn anthropic_models() -> Vec<AnyModel> {
    ANTHROPIC_MODELS.values().cloned().map(AnyModel::Chat).collect()
}

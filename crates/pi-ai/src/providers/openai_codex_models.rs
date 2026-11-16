//! Port of packages/ai/src/providers/openai-codex.models.ts

use std::sync::LazyLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};
use crate::types::AnyModel;

static RAW_CATALOG_JSON: &str = include_str!("data/openai-codex.json");

pub static OPENAI_CODEX_MODELS: LazyLock<ChatModelCatalog> =
    LazyLock::new(|| flatten_chat_catalog_json("openai-codex", RAW_CATALOG_JSON).unwrap_or_default());

pub static OPENAI_CODEX_IMAGE_MODELS: LazyLock<ImageModelCatalog> =
    LazyLock::new(|| flatten_image_catalog_json("openai-codex", RAW_CATALOG_JSON).unwrap_or_default());

pub static OPENAI_CODEX_CLASSIFIER_MODELS: LazyLock<ClassifierModelCatalog> =
    LazyLock::new(|| flatten_classifier_catalog_json("openai-codex", RAW_CATALOG_JSON).unwrap_or_default());

pub fn openai_codex_models() -> Vec<AnyModel> {
    OPENAI_CODEX_MODELS.values().cloned().map(AnyModel::Chat).collect()
}

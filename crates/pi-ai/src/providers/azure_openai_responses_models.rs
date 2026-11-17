//! Port of packages/ai/src/providers/azure-openai-responses.models.ts

use std::sync::LazyLock;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};
use crate::types::AnyModel;

static RAW_CATALOG_JSON: &str = include_str!("data/azure-openai-responses.json");

pub static AZURE_OPENAI_RESPONSES_MODELS: LazyLock<ChatModelCatalog> =
    LazyLock::new(|| flatten_chat_catalog_json("azure-openai-responses", RAW_CATALOG_JSON).unwrap_or_default());

pub static AZURE_OPENAI_RESPONSES_IMAGE_MODELS: LazyLock<ImageModelCatalog> =
    LazyLock::new(|| flatten_image_catalog_json("azure-openai-responses", RAW_CATALOG_JSON).unwrap_or_default());

pub static AZURE_OPENAI_RESPONSES_CLASSIFIER_MODELS: LazyLock<ClassifierModelCatalog> =
    LazyLock::new(|| flatten_classifier_catalog_json("azure-openai-responses", RAW_CATALOG_JSON).unwrap_or_default());

pub fn azure_openai_responses_models() -> Vec<AnyModel> {
    AZURE_OPENAI_RESPONSES_MODELS
        .values()
        .cloned()
        .map(AnyModel::Chat)
        .collect()
}

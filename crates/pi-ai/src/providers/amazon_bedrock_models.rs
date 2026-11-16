//! Port of packages/ai/src/providers/amazon-bedrock.models.ts

use std::sync::LazyLock;

use pi_js::Result;

use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_catalog_json,
    flatten_classifier_catalog_json, flatten_image_catalog_json,
};

const RAW_AMAZON_BEDROCK_CATALOG: &str = include_str!("data/amazon-bedrock.json");

pub fn try_amazon_bedrock_models() -> Result<ChatModelCatalog> {
    flatten_chat_catalog_json("amazon-bedrock", RAW_AMAZON_BEDROCK_CATALOG)
}

pub fn try_amazon_bedrock_image_models() -> Result<ImageModelCatalog> {
    flatten_image_catalog_json("amazon-bedrock", RAW_AMAZON_BEDROCK_CATALOG)
}

pub fn try_amazon_bedrock_classifier_models() -> Result<ClassifierModelCatalog> {
    flatten_classifier_catalog_json("amazon-bedrock", RAW_AMAZON_BEDROCK_CATALOG)
}

pub static AMAZON_BEDROCK_MODELS: LazyLock<ChatModelCatalog> = LazyLock::new(|| match try_amazon_bedrock_models() {
    Ok(models) => models,
    Err(err) => panic!("Failed to load amazon-bedrock chat model catalog: {err}"),
});

pub static AMAZON_BEDROCK_IMAGE_MODELS: LazyLock<ImageModelCatalog> =
    LazyLock::new(|| match try_amazon_bedrock_image_models() {
        Ok(models) => models,
        Err(err) => panic!("Failed to load amazon-bedrock image model catalog: {err}"),
    });

pub static AMAZON_BEDROCK_CLASSIFIER_MODELS: LazyLock<ClassifierModelCatalog> =
    LazyLock::new(|| match try_amazon_bedrock_classifier_models() {
        Ok(models) => models,
        Err(err) => panic!("Failed to load amazon-bedrock classifier model catalog: {err}"),
    });

pub fn amazon_bedrock_models() -> &'static ChatModelCatalog {
    &AMAZON_BEDROCK_MODELS
}

pub fn amazon_bedrock_image_models() -> &'static ImageModelCatalog {
    &AMAZON_BEDROCK_IMAGE_MODELS
}

pub fn amazon_bedrock_classifier_models() -> &'static ClassifierModelCatalog {
    &AMAZON_BEDROCK_CLASSIFIER_MODELS
}

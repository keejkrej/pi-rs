//! Port of packages/ai/src/model-catalog.ts

use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::Value;

use pi_js::{Error, Result};

use crate::types::{ClassifierModel, ImageModel, Model, ModelType};

pub type ModelGroups = IndexMap<String, IndexMap<String, Value>>;

pub type ChatModelCatalog = IndexMap<String, Model>;
pub type ImageModelCatalog = IndexMap<String, ImageModel>;
pub type ClassifierModelCatalog = IndexMap<String, ClassifierModel>;

/// Parse a `providers/data/<provider>.json` catalog JSON string into [`ModelGroups`].
pub fn parse_model_groups_json(raw: &str) -> Result<ModelGroups> {
    pi_js::json::parse_as(raw)
}

/// Extract models matching `model_type` from grouped API maps, keyed by `model.id`.
pub fn flatten_model_catalog(groups: &ModelGroups, model_type: ModelType) -> IndexMap<String, Value> {
    let target = model_type.as_str();
    let mut out = IndexMap::new();
    for models in groups.values() {
        for model in models.values() {
            if model.get("type").and_then(|v| v.as_str()) == Some(target) {
                if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                    out.insert(id.to_owned(), model.clone());
                }
            }
        }
    }
    out
}

fn flatten_typed_catalog<T: DeserializeOwned>(
    groups: &ModelGroups,
    model_type: ModelType,
) -> Result<IndexMap<String, T>> {
    let target = model_type.as_str();
    let mut out = IndexMap::new();
    for models in groups.values() {
        for model in models.values() {
            if model.get("type").and_then(|v| v.as_str()) == Some(target) {
                if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                    let typed: T = serde_json::from_value(model.clone())
                        .map_err(|e| Error::msg(format!("Failed to deserialize {target} model '{id}': {e}")))?;
                    out.insert(id.to_owned(), typed);
                }
            }
        }
    }
    Ok(out)
}

pub fn flatten_chat_model_catalog(_provider: &str, groups: &ModelGroups) -> Result<ChatModelCatalog> {
    flatten_typed_catalog(groups, ModelType::Chat)
}

pub fn flatten_image_model_catalog(_provider: &str, groups: &ModelGroups) -> Result<ImageModelCatalog> {
    flatten_typed_catalog(groups, ModelType::Image)
}

pub fn flatten_classifier_model_catalog(_provider: &str, groups: &ModelGroups) -> Result<ClassifierModelCatalog> {
    flatten_typed_catalog(groups, ModelType::Classifier)
}

pub fn flatten_chat_catalog_json(provider: &str, raw_json: &str) -> Result<ChatModelCatalog> {
    let groups = parse_model_groups_json(raw_json)?;
    flatten_chat_model_catalog(provider, &groups)
}

pub fn flatten_image_catalog_json(provider: &str, raw_json: &str) -> Result<ImageModelCatalog> {
    let groups = parse_model_groups_json(raw_json)?;
    flatten_image_model_catalog(provider, &groups)
}

pub fn flatten_classifier_catalog_json(provider: &str, raw_json: &str) -> Result<ClassifierModelCatalog> {
    let groups = parse_model_groups_json(raw_json)?;
    flatten_classifier_model_catalog(provider, &groups)
}

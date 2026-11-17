//! Port of packages/ai/src/image-models.ts

use std::sync::OnceLock;

use indexmap::IndexMap;

use crate::models_generated::image_models_catalog;
use crate::types::ImageModel;

fn image_models_by_provider() -> &'static IndexMap<String, IndexMap<String, ImageModel>> {
    static BY_PROVIDER: OnceLock<IndexMap<String, IndexMap<String, ImageModel>>> = OnceLock::new();
    BY_PROVIDER.get_or_init(|| {
        let mut out = IndexMap::new();
        for (provider, models) in image_models_catalog() {
            let mut by_id = IndexMap::new();
            for model in models.values() {
                by_id.insert(model.id.clone(), model.clone());
            }
            if !by_id.is_empty() {
                out.insert(provider.clone(), by_id);
            }
        }
        out
    })
}

/// Compat static catalog lookup for image models.
///
/// New code uses `Models::get_model_of_type(ModelType::Image, ...)` or `get_builtin_image_model()`.
pub fn get_image_model(provider: &str, model_id: &str) -> Option<ImageModel> {
    image_models_by_provider()
        .get(provider)
        .and_then(|models| models.get(model_id))
        .cloned()
}

/// Compat static catalog list of providers that define at least one image model.
pub fn get_image_providers() -> Vec<String> {
    image_models_by_provider().keys().cloned().collect()
}

/// Compat static catalog list of image models for a provider.
pub fn get_image_models(provider: &str) -> Vec<ImageModel> {
    image_models_by_provider()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

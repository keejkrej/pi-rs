//! Port of packages/ai/src/images.ts

use std::sync::Once;

use pi_js::{Error, Result};

use crate::images_api_registry::{ImagesApiProvider, get_images_api_provider};
use crate::providers::images::register_builtins::register_built_in_images_api_providers;
use crate::types::{AssistantImages, ImageModel, ImagesContext, ProviderImagesOptions};

fn ensure_builtins_registered() {
    static INIT: Once = Once::new();
    INIT.call_once(register_built_in_images_api_providers);
}

fn resolve_images_api_provider(api: &str) -> Result<ImagesApiProvider> {
    ensure_builtins_registered();
    get_images_api_provider(api).ok_or_else(|| Error::msg(format!("No API provider registered for api: {api}")))
}

/// Global image generation dispatched on `model.api` through the images api
/// registry. Auth must be passed explicitly via `options.api_key`; prefer
/// `Models::generate_images()`, which resolves provider auth.
pub async fn generate_images(
    model: &ImageModel,
    context: &ImagesContext,
    options: Option<ProviderImagesOptions>,
) -> Result<AssistantImages> {
    let provider = resolve_images_api_provider(&model.api)?;
    let opts = options.map(|o| o.base);
    Ok((provider.generate_images)(model.clone(), context.clone(), opts).await?)
}

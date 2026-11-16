//! Port of packages/ai/src/providers/images/register-builtins.ts

use pi_js::BoxFuture;

use crate::images_api_registry::register_images_api_provider;
use crate::types::{AssistantImages, ImageModel, ImagesContext, ImagesFunction, ImagesOptions};
use crate::utils::model_operations::image_error_result_message;

// WIRE(@mcp-w2): When @mcp lands `crate::api::openrouter_images`, delegate
// `generate_images_open_router` to `crate::api::openrouter_images::generate_images`.

pub async fn generate_images_open_router(
    model: &ImageModel,
    _context: &ImagesContext,
    _options: Option<ImagesOptions>,
) -> AssistantImages {
    image_error_result_message(
        model,
        "openrouter-images provider requires @mcp-w2 api::openrouter_images integration",
        false,
    )
}

pub fn register_built_in_images_api_providers() {
    let f: ImagesFunction = std::sync::Arc::new(
        |model: &ImageModel, context: &ImagesContext, options: Option<ImagesOptions>| {
            let model_owned = model.clone();
            let context_owned = context.clone();
            Box::pin(async move { generate_images_open_router(&model_owned, &context_owned, options).await })
                as BoxFuture<AssistantImages>
        },
    );
    register_images_api_provider("openrouter-images", f, None);
}

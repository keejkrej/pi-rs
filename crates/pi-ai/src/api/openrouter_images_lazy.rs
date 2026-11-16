//! Port of packages/ai/src/api/openrouter-images.lazy.ts

use std::sync::Arc;

use crate::types::ProviderImages;

pub fn openrouter_images_api() -> ProviderImages {
    ProviderImages {
        generate_images: Arc::new(|model, context, options| {
            let model = model.clone();
            let context = context.clone();
            Box::pin(async move { crate::api::openrouter_images::generate_images(&model, &context, options).await })
        }),
    }
}

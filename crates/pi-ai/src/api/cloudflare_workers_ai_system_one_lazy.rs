//! Port of packages/ai/src/api/cloudflare-workers-ai-system-one.lazy.ts

use std::sync::Arc;

use crate::types::ProviderClassifier;

pub fn cloudflare_workers_ai_system_one_api() -> ProviderClassifier {
    ProviderClassifier {
        classify: Arc::new(|model, context, options| {
            let model = model.clone();
            let context = context.clone();
            Box::pin(
                async move { crate::api::cloudflare_workers_ai_system_one::classify(&model, &context, options).await },
            )
        }),
    }
}

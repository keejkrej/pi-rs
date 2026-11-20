//! Port of packages/ai/src/api/openai-decisions.lazy.ts

use std::sync::Arc;

use crate::types::ProviderClassifier;

pub fn openai_decisions_api() -> ProviderClassifier {
    ProviderClassifier {
        classify: Arc::new(|model, context, options| {
            let model = model.clone();
            let context = context.clone();
            Box::pin(async move { crate::api::openai_decisions::classify(&model, &context, options).await })
        }),
    }
}

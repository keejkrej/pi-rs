//! Port of packages/ai/src/api/llama-cpp-classify.lazy.ts

use std::sync::Arc;

use crate::types::ProviderClassifier;

pub fn llama_cpp_classify_api() -> ProviderClassifier {
    ProviderClassifier {
        classify: Arc::new(|model, context, options| {
            let model = model.clone();
            let context = context.clone();
            Box::pin(async move { crate::api::llama_cpp_classify::classify(&model, &context, options).await })
        }),
    }
}

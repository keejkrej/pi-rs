//! Port of packages/ai/src/api/typesafe-system-one.lazy.ts

use std::sync::Arc;

use crate::types::ProviderClassifier;

pub fn typesafe_system_one_api() -> ProviderClassifier {
    ProviderClassifier {
        classify: Arc::new(|model, context, options| {
            let model = model.clone();
            let context = context.clone();
            Box::pin(async move { crate::api::typesafe_system_one::classify(&model, &context, options).await })
        }),
    }
}

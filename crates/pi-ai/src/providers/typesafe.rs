//! Port of packages/ai/src/providers/typesafe.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::api::typesafe_system_one_lazy::typesafe_system_one_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderOptions, Provider};
use crate::providers::all::build_provider;
use crate::providers::typesafe_models::typesafe_classifier_models;
use crate::types::AnyModel;

pub fn typesafe_provider() -> Arc<dyn Provider + Send + Sync> {
    let models: Vec<AnyModel> = typesafe_classifier_models()
        .values()
        .cloned()
        .map(AnyModel::Classifier)
        .collect();

    let mut classifiers = IndexMap::new();
    classifiers.insert("typesafe-system-one".to_owned(), typesafe_system_one_api());

    build_provider(CreateProviderOptions {
        id: "typesafe".to_owned(),
        name: Some("TypeSafe".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("TypeSafe API key", &["TYPESAFE_API_KEY"])),
            oauth: None,
        },
        models,
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: None,
        images: None,
        classifiers: Some(classifiers),
    })
}

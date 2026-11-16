//! Port of packages/ai/src/images-api-registry.ts

use std::sync::{Arc, Mutex, OnceLock};

use indexmap::IndexMap;

use pi_js::{BoxFuture, Error, Result};

use crate::types::{AssistantImages, ImageApi, ImageModel, ImagesContext, ImagesFunction, ImagesOptions};

pub type ImagesApiFunction =
    Arc<dyn Fn(ImageModel, ImagesContext, Option<ImagesOptions>) -> BoxFuture<Result<AssistantImages>> + Send + Sync>;

#[derive(Clone)]
pub struct ImagesApiProvider {
    pub api: ImageApi,
    pub generate_images: ImagesApiFunction,
}

#[derive(Clone)]
struct RegisteredImagesApiProvider {
    provider: ImagesApiProvider,
    source_id: Option<String>,
}

static IMAGES_API_REGISTRY: OnceLock<Mutex<IndexMap<String, RegisteredImagesApiProvider>>> = OnceLock::new();

fn registry() -> &'static Mutex<IndexMap<String, RegisteredImagesApiProvider>> {
    IMAGES_API_REGISTRY.get_or_init(|| Mutex::new(IndexMap::new()))
}

fn wrap_generate_images(api: ImageApi, generate_images: ImagesFunction) -> ImagesApiFunction {
    Arc::new(move |model, context, options| {
        let expected = api.clone();
        let inner = Arc::clone(&generate_images);
        Box::pin(async move {
            if model.api != expected {
                return Err(Error::msg(format!("Mismatched api: {} expected {expected}", model.api)));
            }
            Ok(inner(&model, &context, options).await)
        })
    })
}

pub fn register_images_api_provider(
    api: impl Into<ImageApi>,
    generate_images: ImagesFunction,
    source_id: Option<&str>,
) {
    let api = api.into();
    let wrapped = wrap_generate_images(api.clone(), generate_images);
    let entry = RegisteredImagesApiProvider {
        provider: ImagesApiProvider {
            api: api.clone(),
            generate_images: wrapped,
        },
        source_id: source_id.map(str::to_owned),
    };
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(api, entry);
}

pub fn get_images_api_provider(api: &str) -> Option<ImagesApiProvider> {
    let guard = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.get(api).map(|r| r.provider.clone())
}

pub fn unregister_images_api_providers(source_id: &str) {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.retain(|_, v| v.source_id.as_deref() != Some(source_id));
}

pub fn clear_images_api_providers() {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clear();
}

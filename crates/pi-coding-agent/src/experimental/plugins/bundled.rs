//! Port of packages/coding-agent/src/experimental/plugins/bundled.ts

use std::sync::Arc;

use pi_chord::node::{
    FacetBundleArtifact, FacetBundleArtifactLoaderOptions, FacetBundleLoaderOptions, FacetBundleStringOrUrl,
    create_facet_bundle_artifact_loader, create_facet_bundle_loader, read_facet_bundle_manifest,
};
use pi_chord::{FacetLoader, JsonValue, LoadedFacets, combine_facet_loaders};
use serde_json::{Map, Value};

const PRESENTATION_FACET_BUNDLES_KEY: &str = "presentationFacetBundles";
const PI_PLUGIN_API: &str = "@earendil-works/pi-coding-agent/experimental/plugin";

pub fn create_session_plugin_facet_loader(manifest_paths: &[String]) -> Option<FacetLoader> {
    if manifest_paths.is_empty() {
        return None;
    }
    let loaders = manifest_paths
        .iter()
        .cloned()
        .map(create_optional_session_facet_loader)
        .collect();
    Some(combine_facet_loaders(loaders))
}

fn create_optional_session_facet_loader(manifest_path: String) -> FacetLoader {
    FacetLoader::new(Arc::new(move || {
        let manifest_path = manifest_path.clone();
        Box::pin(async move {
            let manifest = read_facet_bundle_manifest(FacetBundleStringOrUrl::String(manifest_path.clone())).await?;
            if manifest.entries.contains_key("session") {
                // PORT: §2.6 JS facet bundles are unsupported; delegate to pi_chord's loader
                // so the canonical unsupported-bundle error is surfaced consistently.
                let loader = create_facet_bundle_loader(FacetBundleLoaderOptions {
                    manifest_path: FacetBundleStringOrUrl::String(manifest_path),
                    entry: "session".to_string(),
                    verify_integrity: None,
                    resolve_external: Some(Arc::new(resolve_plugin_external)),
                })?;
                return loader.load().await;
            }
            Ok(LoadedFacets::new(
                Vec::new(),
                Arc::new(|| Box::pin(futures::future::ready(Ok(())))),
            ))
        })
    }))
}

pub fn create_presentation_facet_data(artifacts: &[FacetBundleArtifact]) -> JsonValue {
    let serialized: Vec<JsonValue> = artifacts
        .iter()
        .filter_map(|artifact| serde_json::to_value(artifact).ok())
        .collect();
    let mut obj = Map::new();
    obj.insert(PRESENTATION_FACET_BUNDLES_KEY.to_string(), Value::Array(serialized));
    Value::Object(obj)
}

/// Create local loaders only from artifacts selected and sent by the connected server.
pub fn create_presentation_facet_loaders(data: &JsonValue) -> pi_js::Result<Vec<FacetLoader>> {
    let Some(obj) = data.as_object() else {
        return Err(pi_js::Error::msg("Invalid presentation plugin data"));
    };
    let Some(artifacts_val) = obj.get(PRESENTATION_FACET_BUNDLES_KEY) else {
        return Ok(Vec::new());
    };
    let Some(artifacts) = artifacts_val.as_array() else {
        return Err(pi_js::Error::msg("Invalid presentation plugin bundle list"));
    };
    let mut loaders = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let artifact = artifact.clone();
        let loader = FacetLoader::new(Arc::new(move || {
            let artifact = artifact.clone();
            Box::pin(async move {
                let inner = create_facet_bundle_artifact_loader(FacetBundleArtifactLoaderOptions {
                    artifact,
                    resolve_external: Some(Arc::new(resolve_plugin_external)),
                    temporary_directory: None,
                })?;
                inner.load().await
            })
        }));
        loaders.push(loader);
    }
    Ok(loaders)
}

fn resolve_plugin_external(specifier: &str) -> Option<FacetBundleStringOrUrl> {
    if specifier != PI_PLUGIN_API {
        return None;
    }
    Some(FacetBundleStringOrUrl::String(PI_PLUGIN_API.to_string()))
}

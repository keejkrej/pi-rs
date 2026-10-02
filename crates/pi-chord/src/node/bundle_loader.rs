//! Port of packages/chord/src/node/bundle-loader.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::node::manifest::{FacetBundleArtifact, FacetBundleEntry, FacetBundleManifest};

// PORT: `string | URL`. `URL` is `url::Url` (`new URL` uses the `url` crate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FacetBundleStringOrUrl {
    String(String),
    Url(url::Url),
}

pub type FacetBundleExternalResolver = Arc<dyn Fn(&str) -> Option<FacetBundleStringOrUrl> + Send + Sync>;

#[derive(Clone)]
pub struct FacetBundleLoaderOptions {
    pub manifest_path: FacetBundleStringOrUrl,
    pub entry: String,
    /// Verify the entry's SHA-256 integrity before evaluating it. Defaults to true.
    pub verify_integrity: Option<bool>,
    /// Resolve host-provided external imports when the bundle is outside the host's package tree.
    pub resolve_external: Option<FacetBundleExternalResolver>,
}

#[derive(Clone)]
pub struct FacetBundleArtifactLoaderOptions {
    pub artifact: serde_json::Value,
    /// Resolve host-provided external imports against the receiving application.
    pub resolve_external: Option<FacetBundleExternalResolver>,
    /// Parent directory for materialized module generations. Defaults to the operating system temp directory.
    pub temporary_directory: Option<String>,
}

// PORT: named form of the inline options object on `readFacetBundleArtifact`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadFacetBundleArtifactOptions {
    pub manifest_path: FacetBundleStringOrUrl,
    pub entry: String,
}

struct CommonJsModule {
    exports: serde_json::Value,
}

/// Read and validate a versioned facet bundle manifest.
pub async fn read_facet_bundle_manifest(path: FacetBundleStringOrUrl) -> pi_js::Result<FacetBundleManifest> {
    todo!("port: read_facet_bundle_manifest")
}

/// Read and verify one transportable entry from a facet bundle on disk.
pub async fn read_facet_bundle_artifact(options: ReadFacetBundleArtifactOptions) -> pi_js::Result<FacetBundleArtifact> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

/// Materialize a transported artifact and create a fresh VM-compiled CommonJS generation for each load.
pub fn create_facet_bundle_artifact_loader(
    options: FacetBundleArtifactLoaderOptions,
) -> pi_js::Result<crate::types::FacetLoader> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

/// Create a reusable loader for one opaque entry in a facet bundle manifest.
pub fn create_facet_bundle_loader(options: FacetBundleLoaderOptions) -> pi_js::Result<crate::types::FacetLoader> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn execute_common_js_module(
    source: &str,
    module_path: &str,
    external_imports: &[String],
    resolver: Option<&FacetBundleExternalResolver>,
) -> pi_js::Result<serde_json::Value> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn to_require_specifier(target: &str) -> pi_js::Result<String> {
    todo!("port: to_require_specifier")
}

fn to_file_path(path: &FacetBundleStringOrUrl) -> pi_js::Result<String> {
    todo!("port: to_file_path")
}

fn resolve_bundle_file(manifest_path: &str, file: &str, label: &str) -> pi_js::Result<String> {
    todo!("port: resolve_bundle_file")
}

fn verify_source(source: &str, entry: &FacetBundleEntry) -> pi_js::Result<()> {
    todo!("port: verify_source")
}

fn parse_integrity(integrity: &str) -> pi_js::Result<String> {
    todo!("port: parse_integrity")
}

fn facets_from_module(
    imported: &serde_json::Value,
    plugin_id: &str,
    entry_name: &str,
) -> pi_js::Result<Vec<crate::types::Facet>> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn validate_artifact(value: &serde_json::Value) -> pi_js::Result<FacetBundleArtifact> {
    todo!("port: validate_artifact")
}

async fn materialize_artifact(directory: &str, artifact: &FacetBundleArtifact) -> pi_js::Result<()> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn resolve_external_target(specifier: &str, resolver: Option<&FacetBundleExternalResolver>) -> pi_js::Result<String> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn validate_package_specifier(specifier: &str) -> pi_js::Result<()> {
    todo!("port: validate_package_specifier")
}

fn validate_manifest(value: &serde_json::Value, path: &str) -> pi_js::Result<FacetBundleManifest> {
    todo!("port: validate_manifest")
}

fn is_record(value: &serde_json::Value) -> bool {
    todo!("port: is_record")
}

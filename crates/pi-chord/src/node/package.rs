//! Port of packages/chord/src/node/package.ts

#![allow(dead_code, unused_variables)]

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::node::manifest::FacetBundleManifest;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFacetPackageOptions {
    /// Plugin package directory or its package.json path.
    pub package_path: String,
    pub outdir: String,
    /// Application conventions applied when the corresponding source file exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_facets: Option<IndexMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFacetPackageResult {
    pub manifest: FacetBundleManifest,
    pub manifest_path: String,
    pub package_directory: String,
    pub package_json_path: String,
}

struct FacetPackageMetadata {
    package_directory: String,
    package_json_path: String,
    name: String,
    version: String,
    peer_dependencies: Vec<String>,
    configured_facets: IndexMap<String, ConfiguredFacet>,
    external: Vec<String>,
    source_map: bool,
}

// PORT: `string | false` in package.json `chord.facets`.
enum ConfiguredFacet {
    Path(String),
    Disabled,
}

struct ChordPackageConfiguration {
    facets: IndexMap<String, ConfiguredFacet>,
    external: Vec<String>,
    source_map: bool,
}

/// Build a plugin package using package.json metadata and application-provided facet conventions.
pub async fn bundle_facet_package(options: BundleFacetPackageOptions) -> pi_js::Result<BundleFacetPackageResult> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

async fn read_facet_package_metadata(package_path: &str) -> pi_js::Result<FacetPackageMetadata> {
    todo!("port: read_facet_package_metadata")
}

// PORT: JS `undefined` is `None`; JSON `null` is `Some(Value::Null)`.
fn parse_peer_dependencies(value: Option<&serde_json::Value>, package_json_path: &str) -> pi_js::Result<Vec<String>> {
    todo!("port: parse_peer_dependencies")
}

// PORT: JS `undefined` is `None`; JSON `null` is `Some(Value::Null)`.
fn parse_chord_configuration(
    value: Option<&serde_json::Value>,
    package_json_path: &str,
) -> pi_js::Result<ChordPackageConfiguration> {
    todo!("port: parse_chord_configuration")
}

async fn resolve_facet_entries(
    metadata: &FacetPackageMetadata,
    default_facets: &IndexMap<String, String>,
) -> pi_js::Result<IndexMap<String, String>> {
    todo!("port: resolve_facet_entries")
}

fn validate_facet_mapping(name: &str, source: &str, kind: &str) -> pi_js::Result<()> {
    todo!("port: validate_facet_mapping")
}

fn resolve_package_entry(package_directory: &str, source: &str, name: &str) -> pi_js::Result<String> {
    todo!("port: resolve_package_entry")
}

fn validate_canonical_package_entry(package_directory: &str, path: &str, name: &str) -> pi_js::Result<()> {
    todo!("port: validate_canonical_package_entry")
}

// PORT: caught values are `pi_js::Error`; `error.code === "ENOENT"` is `error.code() == Some("ENOENT")`.
fn is_missing_path(error: &pi_js::Error) -> bool {
    todo!("port: is_missing_path")
}

fn is_record(value: &serde_json::Value) -> bool {
    todo!("port: is_record")
}

//! Port of packages/chord/src/node/manifest.ts

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub const FACET_BUNDLE_FORMAT: &str = "chord.facet-bundle";
pub const FACET_BUNDLE_FORMAT_VERSION: i64 = 2;
pub const FACET_BUNDLE_MANIFEST_FILE: &str = "chord-facets.json";
pub const FACET_BUNDLE_ARTIFACT_FORMAT: &str = "chord.facet-bundle-artifact";
pub const FACET_BUNDLE_ARTIFACT_FORMAT_VERSION: i64 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FacetBundleEntry {
    /// Content-addressed CommonJS filename relative to the manifest.
    pub file: String,
    /// SHA-256 subresource-integrity value for the JavaScript file.
    pub integrity: String,
    /// Imports intentionally left for the loading application to resolve.
    pub external_imports: Vec<String>,
    /// Source map filename relative to the manifest, when emitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FacetBundleManifest {
    pub format: String,
    pub format_version: i64,
    pub plugin: FacetBundlePlugin,
    pub entries: IndexMap<String, FacetBundleEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FacetBundlePlugin {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// One self-contained manifest entry suitable for storage or transport to another Node host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FacetBundleArtifact {
    pub format: String,
    pub format_version: i64,
    pub plugin: FacetBundlePlugin,
    pub entry_name: String,
    pub entry: FacetBundleEntry,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map_contents: Option<String>,
}

//! Port of packages/chord/src/node/bundle.ts

#![allow(dead_code, unused_variables)]

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::node::manifest::{FacetBundleEntry, FacetBundleManifest, FacetBundlePlugin};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FacetBundlePlatform {
    #[serde(rename = "node")]
    Node,
    #[serde(rename = "browser")]
    Browser,
    #[serde(rename = "neutral")]
    Neutral,
}

// PORT: `string | readonly string[]`. The string arm is checked first, matching bundle.ts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FacetBundleTarget {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFacetsOptions {
    pub plugin: FacetBundlePlugin,
    /// Opaque application-selected entry names mapped to TypeScript or JavaScript source files.
    pub entries: IndexMap<String, String>,
    pub outdir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Additional package imports intentionally left for the loading application to resolve.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minify: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub define: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<FacetBundlePlatform>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<FacetBundleTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFacetsResult {
    pub manifest: FacetBundleManifest,
    pub manifest_path: String,
}

struct BundleEntryInput<'a> {
    entry_name: &'a str,
    source: &'a str,
    temporary_directory: &'a str,
    working_directory: &'a str,
    options: &'a BundleFacetsOptions,
}

// PORT: esbuild is not a dependency. Fields are the `Message` slice this file reads.
struct EsbuildLocation {
    file: String,
    line: i64,
    column: i64,
}

struct EsbuildMessage {
    text: String,
    location: Option<EsbuildLocation>,
}

/// Bundle each opaque facet entry into an independent content-addressed CommonJS file.
pub async fn bundle_facets(options: BundleFacetsOptions) -> pi_js::Result<BundleFacetsResult> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

async fn bundle_entry(input: BundleEntryInput<'_>) -> pi_js::Result<FacetBundleEntry> {
    Err(pi_js::Error::js(
        "Error",
        "JavaScript facet bundles are not supported by pi-rs",
    ))
}

fn validate_options(options: &BundleFacetsOptions) -> pi_js::Result<()> {
    if options.plugin.id.is_empty() {
        return Err(pi_js::Error::js(
            "TypeError",
            "Facet bundle plugin ID must not be empty",
        ));
    }
    if options
        .plugin
        .version
        .as_ref()
        .is_some_and(|version| version.is_empty())
    {
        return Err(pi_js::Error::js(
            "TypeError",
            "Facet bundle plugin version must not be empty",
        ));
    }
    if options.entries.is_empty() {
        return Err(pi_js::Error::js(
            "TypeError",
            "Facet bundle must contain at least one entry",
        ));
    }
    for (name, source) in &options.entries {
        if name.is_empty() {
            return Err(pi_js::Error::js(
                "TypeError",
                "Facet bundle entry name must not be empty",
            ));
        }
        if source.is_empty() {
            return Err(pi_js::Error::js(
                "TypeError",
                format!("Facet bundle entry {name} must have a source path"),
            ));
        }
    }
    if let Some(externals) = &options.external {
        for external in externals {
            if external.is_empty() {
                return Err(pi_js::Error::js(
                    "TypeError",
                    "Facet bundle external import must not be empty",
                ));
            }
        }
    }
    Ok(())
}

async fn replace_directory(temporary_directory: &str, output_directory: &str) -> pi_js::Result<()> {
    let backup_directory = format!("{output_directory}.old-{}", pi_js::crypto::random_uuid());
    let mut moved_existing = false;
    match pi_js::fs::promises::rename(output_directory, &backup_directory).await {
        Ok(()) => moved_existing = true,
        Err(error) if is_missing_path(&error) => {}
        Err(error) => return Err(error),
    }
    if let Err(error) = pi_js::fs::promises::rename(temporary_directory, output_directory).await {
        if moved_existing {
            pi_js::fs::promises::rename(&backup_directory, output_directory).await?;
        }
        return Err(error);
    }
    if moved_existing {
        pi_js::fs::promises::rm(&backup_directory, true, true).await?;
    }
    Ok(())
}

fn short_hash(value: &str) -> String {
    let digest = pi_js::crypto::sha256_hex(value.as_bytes());
    digest.chars().take(12).collect()
}

fn esbuild_messages(error: &serde_json::Value) -> Vec<String> {
    let Some(obj) = error.as_object() else {
        return Vec::new();
    };
    let Some(errors) = obj.get("errors").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    errors
        .iter()
        .filter(|candidate| is_esbuild_message(candidate))
        .filter_map(|candidate| {
            let text = candidate.get("text")?.as_str()?.to_string();
            let location = match candidate.get("location") {
                Some(serde_json::Value::Null) | None => None,
                Some(loc) => Some(EsbuildLocation {
                    file: loc.get("file").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    line: loc.get("line").and_then(|v| v.as_i64()).unwrap_or_default(),
                    column: loc.get("column").and_then(|v| v.as_i64()).unwrap_or_default(),
                }),
            };
            Some(format_esbuild_message(&EsbuildMessage { text, location }))
        })
        .collect()
}

fn format_esbuild_message(message: &EsbuildMessage) -> String {
    match &message.location {
        None => message.text.clone(),
        Some(location) => format!(
            "{}:{}:{}: {}",
            location.file,
            location.line,
            location.column + 1,
            message.text
        ),
    }
}

fn is_esbuild_message(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let has_text = obj.get("text").is_some_and(|v| v.is_string());
    let has_location = obj.get("location").is_some_and(|loc| loc.is_null() || is_record(loc));
    has_text && has_location
}

// PORT: caught values are `pi_js::Error`; `error.code === "ENOENT"` is `error.code() == Some("ENOENT")`.
fn is_missing_path(error: &pi_js::Error) -> bool {
    error.code() == Some("ENOENT")
}

fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

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
    todo!("port: validate_options")
}

async fn replace_directory(temporary_directory: &str, output_directory: &str) -> pi_js::Result<()> {
    todo!("port: replace_directory")
}

fn short_hash(value: &str) -> String {
    todo!("port: short_hash")
}

fn esbuild_messages(error: &serde_json::Value) -> Vec<String> {
    todo!("port: esbuild_messages")
}

fn format_esbuild_message(message: &EsbuildMessage) -> String {
    todo!("port: format_esbuild_message")
}

fn is_esbuild_message(value: &serde_json::Value) -> bool {
    todo!("port: is_esbuild_message")
}

// PORT: caught values are `pi_js::Error`; `error.code === "ENOENT"` is `error.code() == Some("ENOENT")`.
fn is_missing_path(error: &pi_js::Error) -> bool {
    todo!("port: is_missing_path")
}

fn is_record(value: &serde_json::Value) -> bool {
    todo!("port: is_record")
}

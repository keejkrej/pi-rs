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
    if package_path.is_empty() {
        return Err(pi_js::Error::js("TypeError", "Facet package path must not be empty"));
    }
    let candidate = pi_js::path::resolve(&[package_path]);
    let candidate_stats = pi_js::fs::promises::stat(&candidate).await.map_err(|error| {
        pi_js::Error::js_with_cause("Error", format!("Could not access facet package {candidate}"), error)
    })?;
    let (package_directory, package_json_path) = if candidate_stats.is_directory() {
        let dir = pi_js::fs::promises::realpath(&candidate).await?;
        let pkg = pi_js::path::join(&[&dir, "package.json"]);
        (dir, pkg)
    } else if candidate_stats.is_file() && pi_js::path::basename(&candidate, None) == "package.json" {
        let pkg = pi_js::fs::promises::realpath(&candidate).await?;
        let dir = pi_js::path::dirname(&pkg);
        (dir, pkg)
    } else {
        return Err(pi_js::Error::msg(format!(
            "Facet package path must name a directory or package.json: {candidate}"
        )));
    };

    let contents = pi_js::fs::promises::read_to_string(&package_json_path)
        .await
        .map_err(|error| {
            pi_js::Error::js_with_cause(
                "Error",
                format!("Could not read facet package metadata {package_json_path}"),
                error,
            )
        })?;
    let parsed = pi_js::json::parse(&contents).map_err(|error| {
        pi_js::Error::js_with_cause(
            "Error",
            format!("Could not read facet package metadata {package_json_path}"),
            error,
        )
    })?;
    let Some(obj) = parsed.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Facet package metadata must be an object: {package_json_path}"
        )));
    };
    let Some(name) = obj.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg(format!(
            "Facet package must have a non-empty name: {package_json_path}"
        )));
    };
    let Some(version) = obj.get("version").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
        return Err(pi_js::Error::msg(format!(
            "Facet package must have a non-empty version: {package_json_path}"
        )));
    };
    let peer_dependencies = parse_peer_dependencies(obj.get("peerDependencies"), &package_json_path)?;
    let chord = parse_chord_configuration(obj.get("chord"), &package_json_path)?;
    Ok(FacetPackageMetadata {
        package_directory,
        package_json_path,
        name: name.to_string(),
        version: version.to_string(),
        peer_dependencies,
        configured_facets: chord.facets,
        external: chord.external,
        source_map: chord.source_map,
    })
}

// PORT: JS `undefined` is `None`; JSON `null` is `Some(Value::Null)`.
fn parse_peer_dependencies(value: Option<&serde_json::Value>, package_json_path: &str) -> pi_js::Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(obj) = value.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Facet package has invalid peerDependencies: {package_json_path}"
        )));
    };
    let mut keys = Vec::with_capacity(obj.len());
    for (name, version) in obj {
        if name.is_empty() || !version.is_string() {
            return Err(pi_js::Error::msg(format!(
                "Facet package has invalid peerDependencies: {package_json_path}"
            )));
        }
        keys.push(name.clone());
    }
    keys.sort();
    Ok(keys)
}

// PORT: JS `undefined` is `None`; JSON `null` is `Some(Value::Null)`.
fn parse_chord_configuration(
    value: Option<&serde_json::Value>,
    package_json_path: &str,
) -> pi_js::Result<ChordPackageConfiguration> {
    let Some(value) = value else {
        return Ok(ChordPackageConfiguration {
            facets: IndexMap::new(),
            external: Vec::new(),
            source_map: true,
        });
    };
    let Some(obj) = value.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Facet package chord configuration must be an object: {package_json_path}"
        )));
    };
    if obj
        .keys()
        .any(|key| key != "facets" && key != "external" && key != "sourceMap")
    {
        return Err(pi_js::Error::msg(format!(
            "Facet package chord configuration has an unknown field: {package_json_path}"
        )));
    }
    let mut facets = IndexMap::new();
    if let Some(facets_val) = obj.get("facets") {
        let Some(facets_obj) = facets_val.as_object() else {
            return Err(pi_js::Error::msg(format!(
                "Facet package chord.facets must be an object: {package_json_path}"
            )));
        };
        for (name, source) in facets_obj {
            if name.is_empty() {
                return Err(pi_js::Error::msg(format!(
                    "Facet package has an invalid chord.facets entry: {package_json_path}"
                )));
            }
            match source {
                serde_json::Value::Bool(false) => {
                    facets.insert(name.clone(), ConfiguredFacet::Disabled);
                }
                serde_json::Value::String(s) if !s.is_empty() => {
                    facets.insert(name.clone(), ConfiguredFacet::Path(s.clone()));
                }
                _ => {
                    return Err(pi_js::Error::msg(format!(
                        "Facet package has an invalid chord.facets entry: {package_json_path}"
                    )));
                }
            }
        }
    }
    let mut external = Vec::new();
    if let Some(ext_val) = obj.get("external") {
        let Some(ext_arr) = ext_val.as_array() else {
            return Err(pi_js::Error::msg(format!(
                "Facet package chord.external must contain non-empty strings: {package_json_path}"
            )));
        };
        let mut set = indexmap::IndexSet::new();
        for item in ext_arr {
            let Some(s) = item.as_str().filter(|s| !s.is_empty()) else {
                return Err(pi_js::Error::msg(format!(
                    "Facet package chord.external must contain non-empty strings: {package_json_path}"
                )));
            };
            set.insert(s.to_string());
        }
        external.extend(set);
        external.sort();
    }
    let source_map = match obj.get("sourceMap") {
        None => true,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => {
            return Err(pi_js::Error::msg(format!(
                "Facet package chord.sourceMap must be a boolean: {package_json_path}"
            )));
        }
    };
    Ok(ChordPackageConfiguration {
        facets,
        external,
        source_map,
    })
}

async fn resolve_facet_entries(
    metadata: &FacetPackageMetadata,
    default_facets: &IndexMap<String, String>,
) -> pi_js::Result<IndexMap<String, String>> {
    let mut entries = IndexMap::new();
    for (name, source) in default_facets {
        validate_facet_mapping(name, source, "default")?;
        let path = resolve_package_entry(&metadata.package_directory, source, name)?;
        match pi_js::fs::promises::stat(&path).await {
            Ok(entry_stats) => {
                if !entry_stats.is_file() {
                    return Err(pi_js::Error::msg(format!(
                        "Default facet entry {name} is not a file: {path}"
                    )));
                }
                let canonical_path = pi_js::fs::promises::realpath(&path).await?;
                validate_canonical_package_entry(&metadata.package_directory, &canonical_path, name)?;
                entries.insert(name.clone(), canonical_path);
            }
            Err(error) if is_missing_path(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    for (name, configured) in &metadata.configured_facets {
        let source = match configured {
            ConfiguredFacet::Disabled => {
                entries.shift_remove(name);
                continue;
            }
            ConfiguredFacet::Path(source) => source,
        };
        validate_facet_mapping(name, source, "configured")?;
        let path = resolve_package_entry(&metadata.package_directory, source, name)?;
        let entry_stats = pi_js::fs::promises::stat(&path).await.map_err(|error| {
            pi_js::Error::js_with_cause(
                "Error",
                format!("Could not access configured facet entry {name}: {path}"),
                error,
            )
        })?;
        if !entry_stats.is_file() {
            return Err(pi_js::Error::msg(format!(
                "Configured facet entry {name} is not a file: {path}"
            )));
        }
        let canonical_path = pi_js::fs::promises::realpath(&path).await?;
        validate_canonical_package_entry(&metadata.package_directory, &canonical_path, name)?;
        entries.insert(name.clone(), canonical_path);
    }
    if entries.is_empty() {
        return Err(pi_js::Error::msg(format!(
            "Facet package {} has no configured or conventional facet entries",
            metadata.name
        )));
    }
    Ok(entries)
}

fn validate_facet_mapping(name: &str, source: &str, kind: &str) -> pi_js::Result<()> {
    if name.is_empty() {
        return Err(pi_js::Error::msg(format!(
            "Facet package {kind} entry name must not be empty"
        )));
    }
    if source.is_empty() {
        return Err(pi_js::Error::msg(format!(
            "Facet package {kind} entry {name} must have a source path"
        )));
    }
    Ok(())
}

fn resolve_package_entry(package_directory: &str, source: &str, name: &str) -> pi_js::Result<String> {
    if pi_js::path::is_absolute(source) {
        return Err(pi_js::Error::msg(format!(
            "Facet package entry {name} must be relative to the package directory"
        )));
    }
    let path = pi_js::path::resolve(&[package_directory, source]);
    let relative_path = pi_js::path::relative(package_directory, &path);
    let prefix = format!("..{}", pi_js::path::sep());
    if relative_path.is_empty()
        || relative_path == ".."
        || relative_path.starts_with(&prefix)
        || pi_js::path::is_absolute(&relative_path)
    {
        return Err(pi_js::Error::msg(format!(
            "Facet package entry {name} escapes the package directory"
        )));
    }
    Ok(path)
}

fn validate_canonical_package_entry(package_directory: &str, path: &str, name: &str) -> pi_js::Result<()> {
    let relative_path = pi_js::path::relative(package_directory, path);
    let prefix = format!("..{}", pi_js::path::sep());
    if relative_path == ".." || relative_path.starts_with(&prefix) || pi_js::path::is_absolute(&relative_path) {
        return Err(pi_js::Error::msg(format!(
            "Facet package entry {name} resolves outside the package directory"
        )));
    }
    Ok(())
}

// PORT: caught values are `pi_js::Error`; `error.code === "ENOENT"` is `error.code() == Some("ENOENT")`.
fn is_missing_path(error: &pi_js::Error) -> bool {
    error.code() == Some("ENOENT")
}

fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

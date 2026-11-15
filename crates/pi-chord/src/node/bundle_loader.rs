//! Port of packages/chord/src/node/bundle-loader.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::node::manifest::{
    FACET_BUNDLE_ARTIFACT_FORMAT, FACET_BUNDLE_ARTIFACT_FORMAT_VERSION, FACET_BUNDLE_FORMAT,
    FACET_BUNDLE_FORMAT_VERSION, FacetBundleArtifact, FacetBundleEntry, FacetBundleManifest, FacetBundlePlugin,
};

const NODE_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "inspector/promises",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "sqlite",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "test",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

fn is_builtin(target: &str) -> bool {
    let bare = target.strip_prefix("node:").unwrap_or(target);
    NODE_BUILTINS.contains(&bare)
}

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
    let manifest_path = to_file_path(&path)?;
    let contents = pi_js::fs::promises::read_to_string(&manifest_path)
        .await
        .map_err(|error| {
            pi_js::Error::js_with_cause(
                "Error",
                format!("Could not read facet bundle manifest {manifest_path}"),
                error,
            )
        })?;
    let parsed = pi_js::json::parse(&contents).map_err(|error| {
        pi_js::Error::js_with_cause(
            "Error",
            format!("Could not read facet bundle manifest {manifest_path}"),
            error,
        )
    })?;
    validate_manifest(&parsed, &manifest_path)
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
    if pi_js::path::is_absolute(target) || is_builtin(target) {
        return Ok(target.to_string());
    }
    let Ok(url) = url::Url::parse(target) else {
        return Ok(target.to_string());
    };
    if url.scheme() == "file" {
        return url
            .to_file_path()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|()| {
                pi_js::Error::msg(format!(
                    "Facet bundle external target must be a file, package, or built-in module: {target}"
                ))
            });
    }
    if url.scheme() == "node" {
        return Ok(url.to_string());
    }
    Err(pi_js::Error::msg(format!(
        "Facet bundle external target must be a file, package, or built-in module: {target}"
    )))
}

fn to_file_path(path: &FacetBundleStringOrUrl) -> pi_js::Result<String> {
    match path {
        FacetBundleStringOrUrl::String(s) => Ok(pi_js::path::resolve(&[s])),
        FacetBundleStringOrUrl::Url(url) => {
            if url.scheme() != "file" {
                return Err(pi_js::Error::js(
                    "TypeError",
                    format!("Facet bundle manifest must be a file URL, not {}:", url.scheme()),
                ));
            }
            url.to_file_path()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|()| pi_js::Error::js("TypeError", format!("Invalid file URL: {url}")))
        }
    }
}

fn resolve_bundle_file(manifest_path: &str, file: &str, label: &str) -> pi_js::Result<String> {
    if file.is_empty()
        || pi_js::path::is_absolute(file)
        || pi_js::path::basename(file, None) != file
        || file == "."
        || file == ".."
    {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle {label} must be a filename relative to its manifest"
        )));
    }
    let dir = pi_js::path::dirname(manifest_path);
    Ok(pi_js::path::resolve(&[&dir, file]))
}

fn verify_source(source: &str, entry: &FacetBundleEntry) -> pi_js::Result<()> {
    let expected = parse_integrity(&entry.integrity)?;
    let actual = pi_js::b64::encode(&pi_js::crypto::sha256(source.as_bytes()));
    if actual != expected {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle integrity check failed for {}",
            entry.file
        )));
    }
    Ok(())
}

fn parse_integrity(integrity: &str) -> pi_js::Result<String> {
    let prefix = "sha256-";
    if !integrity.starts_with(prefix) || integrity.len() == prefix.len() {
        return Err(pi_js::Error::msg(
            "Facet bundle entry has an invalid SHA-256 integrity value",
        ));
    }
    Ok(integrity[prefix.len()..].to_string())
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
    let Some(obj) = value.as_object() else {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    };
    let Some(format) = obj.get("format").and_then(|v| v.as_str()) else {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    };
    let Some(format_version) = obj.get("formatVersion").and_then(|v| v.as_i64()) else {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    };
    let Some(entry_name) = obj.get("entryName").and_then(|v| v.as_str()) else {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    };
    let Some(source) = obj.get("source").and_then(|v| v.as_str()) else {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    };
    if format != FACET_BUNDLE_ARTIFACT_FORMAT
        || format_version != FACET_BUNDLE_ARTIFACT_FORMAT_VERSION
        || entry_name.is_empty()
    {
        return Err(pi_js::Error::msg("Invalid facet bundle artifact"));
    }

    let mut entries_obj = serde_json::Map::new();
    entries_obj.insert(
        entry_name.to_string(),
        obj.get("entry").cloned().unwrap_or(serde_json::Value::Null),
    );
    let mut synthetic_manifest = serde_json::Map::new();
    synthetic_manifest.insert(
        "format".to_string(),
        serde_json::Value::String(FACET_BUNDLE_FORMAT.to_string()),
    );
    synthetic_manifest.insert(
        "formatVersion".to_string(),
        serde_json::Value::Number(FACET_BUNDLE_FORMAT_VERSION.into()),
    );
    if let Some(plugin) = obj.get("plugin") {
        synthetic_manifest.insert("plugin".to_string(), plugin.clone());
    }
    synthetic_manifest.insert("entries".to_string(), serde_json::Value::Object(entries_obj));

    let manifest = validate_manifest(&serde_json::Value::Object(synthetic_manifest), "facet bundle artifact")?;
    let entry = manifest
        .entries
        .get(entry_name)
        .cloned()
        .ok_or_else(|| pi_js::Error::msg("Invalid facet bundle artifact"))?;

    let source_map_contents = obj.get("sourceMapContents");
    if entry.source_map.is_none() {
        if source_map_contents.is_some() {
            return Err(pi_js::Error::msg(
                "Facet bundle artifact has source map contents without a source map",
            ));
        }
    } else if !source_map_contents.is_some_and(|v| v.is_string()) {
        return Err(pi_js::Error::msg(
            "Facet bundle artifact is missing its source map contents",
        ));
    }

    verify_source(source, &entry)?;

    Ok(FacetBundleArtifact {
        format: FACET_BUNDLE_ARTIFACT_FORMAT.to_string(),
        format_version: FACET_BUNDLE_ARTIFACT_FORMAT_VERSION,
        plugin: manifest.plugin,
        entry_name: entry_name.to_string(),
        entry,
        source: source.to_string(),
        source_map_contents: source_map_contents.and_then(|v| v.as_str()).map(str::to_string),
    })
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
    let segments: Vec<&str> = specifier.split('/').collect();
    let package_parts = if specifier.starts_with('@') { 2 } else { 1 };
    if specifier.starts_with('.')
        || specifier.starts_with('/')
        || specifier.starts_with('#')
        || specifier.contains(':')
        || specifier.contains('\\')
        || segments.len() < package_parts
        || segments
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle artifact has an unsupported external import: {specifier}"
        )));
    }
    Ok(())
}

fn validate_manifest(value: &serde_json::Value, path: &str) -> pi_js::Result<FacetBundleManifest> {
    let Some(obj) = value.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Invalid facet bundle manifest format in {path}"
        )));
    };
    if obj.get("format").and_then(|v| v.as_str()) != Some(FACET_BUNDLE_FORMAT) {
        return Err(pi_js::Error::msg(format!(
            "Invalid facet bundle manifest format in {path}"
        )));
    }
    let format_version = obj.get("formatVersion");
    if format_version.and_then(|v| v.as_i64()) != Some(FACET_BUNDLE_FORMAT_VERSION) {
        let ver_str = match format_version {
            None => "undefined".to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        return Err(pi_js::Error::msg(format!(
            "Unsupported facet bundle manifest version in {path}: {ver_str}"
        )));
    }
    let Some(plugin_obj) = obj.get("plugin").and_then(|v| v.as_object()) else {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle manifest has an invalid plugin identity in {path}"
        )));
    };
    let Some(plugin_id) = plugin_obj.get("id").and_then(|v| v.as_str()) else {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle manifest has an invalid plugin identity in {path}"
        )));
    };
    if plugin_id.is_empty() {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle manifest has an invalid plugin identity in {path}"
        )));
    }
    let plugin_version = match plugin_obj.get("version") {
        None => None,
        Some(v) => {
            let Some(s) = v.as_str() else {
                return Err(pi_js::Error::msg(format!(
                    "Facet bundle manifest has an invalid plugin version in {path}"
                )));
            };
            if s.is_empty() {
                return Err(pi_js::Error::msg(format!(
                    "Facet bundle manifest has an invalid plugin version in {path}"
                )));
            }
            Some(s.to_string())
        }
    };
    let Some(entries_obj) = obj.get("entries").and_then(|v| v.as_object()) else {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle manifest has no entries in {path}"
        )));
    };
    if entries_obj.is_empty() {
        return Err(pi_js::Error::msg(format!(
            "Facet bundle manifest has no entries in {path}"
        )));
    }

    let mut entries = indexmap::IndexMap::new();
    for (name, candidate) in entries_obj {
        let Some(candidate_obj) = candidate.as_object() else {
            return Err(pi_js::Error::msg(format!(
                "Facet bundle manifest has an invalid entry in {path}"
            )));
        };
        if name.is_empty() {
            return Err(pi_js::Error::msg(format!(
                "Facet bundle manifest has an invalid entry in {path}"
            )));
        }
        let Some(file) = candidate_obj.get("file").and_then(|v| v.as_str()) else {
            return Err(pi_js::Error::msg(format!("Facet bundle entry {name} has no file")));
        };
        resolve_bundle_file(path, file, &format!("entry {name}"))?;
        let Some(integrity) = candidate_obj.get("integrity").and_then(|v| v.as_str()) else {
            return Err(pi_js::Error::msg(format!("Facet bundle entry {name} has no integrity")));
        };
        parse_integrity(integrity)?;
        let Some(external_arr) = candidate_obj.get("externalImports").and_then(|v| v.as_array()) else {
            return Err(pi_js::Error::msg(format!(
                "Facet bundle entry {name} has invalid external imports"
            )));
        };
        let mut external_imports = Vec::with_capacity(external_arr.len());
        let mut seen = indexmap::IndexSet::new();
        for item in external_arr {
            let Some(s) = item.as_str() else {
                return Err(pi_js::Error::msg(format!(
                    "Facet bundle entry {name} has invalid external imports"
                )));
            };
            if !seen.insert(s) {
                return Err(pi_js::Error::msg(format!(
                    "Facet bundle entry {name} has duplicate external imports"
                )));
            }
            external_imports.push(s.to_string());
        }
        let source_map = match candidate_obj.get("sourceMap") {
            None => None,
            Some(v) => {
                let Some(s) = v.as_str() else {
                    return Err(pi_js::Error::msg(format!(
                        "Facet bundle entry {name} has an invalid source map"
                    )));
                };
                resolve_bundle_file(path, s, &format!("entry {name} source map"))?;
                Some(s.to_string())
            }
        };
        entries.insert(
            name.clone(),
            FacetBundleEntry {
                file: file.to_string(),
                integrity: integrity.to_string(),
                external_imports,
                source_map,
            },
        );
    }

    Ok(FacetBundleManifest {
        format: FACET_BUNDLE_FORMAT.to_string(),
        format_version: FACET_BUNDLE_FORMAT_VERSION,
        plugin: FacetBundlePlugin {
            id: plugin_id.to_string(),
            version: plugin_version,
        },
        entries,
    })
}

fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

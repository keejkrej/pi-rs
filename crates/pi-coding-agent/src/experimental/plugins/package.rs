//! Port of packages/coding-agent/src/experimental/plugins/package.ts

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use pi_chord::bundler::{BundleFacetPackageOptions, bundle_facet_package};
use pi_chord::node::{
    FACET_BUNDLE_MANIFEST_FILE, FacetBundleArtifact, FacetBundleStringOrUrl, ReadFacetBundleArtifactOptions,
    read_facet_bundle_artifact,
};
use pi_js::crypto::sha256_hex;
use pi_js::fs::promises::{read_to_string, rm, write_file};
use pi_js::json::{parse, stringify_pretty};
use pi_js::path::{basename, dirname, join};
use pi_protocol::ServerId;
use serde_json::{Map, Value};
use tokio::sync::Mutex as AsyncMutex;

use crate::utils::paths::{PathInputOptions, resolve_path};

const PLUGIN_PACKAGE_PROFILE_VERSION: i64 = 1;

fn default_plugin_facets() -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    map.insert("session".to_string(), "src/session.ts".to_string());
    map.insert("tui".to_string(), "src/tui.ts".to_string());
    map
}

#[derive(Clone)]
pub struct ConfiguredServerPluginPackage {
    pub manifest_path: String,
    package_path: String,
    outdir: String,
    build_lock: Arc<AsyncMutex<()>>,
}

impl ConfiguredServerPluginPackage {
    pub async fn build(&self) -> pi_js::Result<Vec<FacetBundleArtifact>> {
        let _guard = self.build_lock.lock().await;
        // PORT: §2.6 JavaScript facet bundling is unsupported in pi-rs; calling into
        // `pi_chord::bundler::bundle_facet_package` surfaces the canonical error.
        let result = bundle_facet_package(BundleFacetPackageOptions {
            package_path: self.package_path.clone(),
            outdir: self.outdir.clone(),
            default_facets: Some(default_plugin_facets()),
        })
        .await?;
        if !result.manifest.entries.contains_key("tui") {
            return Ok(Vec::new());
        }
        let artifact = read_facet_bundle_artifact(ReadFacetBundleArtifactOptions {
            manifest_path: FacetBundleStringOrUrl::String(result.manifest_path),
            entry: "tui".to_string(),
        })
        .await?;
        Ok(vec![artifact])
    }
}

/// Persist an explicit plugin package selection or restore it for a later server generation.
pub async fn restore_server_plugin_package_profile(
    directory: &str,
    server_id: &ServerId,
    configured_package_paths: Option<&[String]>,
) -> pi_js::Result<Vec<String>> {
    let path = join(&[directory, &format!("plugin-packages-{server_id}.json")]);
    let Some(configured) = configured_package_paths else {
        return Ok(read_plugin_package_profile(&path, false, None)
            .await?
            .unwrap_or_default());
    };
    let package_paths = normalize_plugin_package_paths(configured)?;
    if package_paths.is_empty() {
        rm(&path, false, true).await?;
    } else {
        write_plugin_package_profile(&path, &package_paths, None).await?;
    }
    Ok(package_paths)
}

/// Read the explicit plugin selection stored for one durable Session.
pub async fn read_session_plugin_package_profile(
    directory: &str,
    server_id: &ServerId,
    session_path: &str,
) -> pi_js::Result<Option<Vec<String>>> {
    let profile_path = session_plugin_profile_path(directory, server_id, session_path);
    read_plugin_package_profile(&profile_path, true, Some(session_path)).await
}

/// Remove the plugin selection stored for one deleted Session.
pub async fn remove_session_plugin_package_profile(
    directory: &str,
    server_id: &ServerId,
    session_path: &str,
) -> pi_js::Result<()> {
    let profile_path = session_plugin_profile_path(directory, server_id, session_path);
    rm(&profile_path, false, true).await
}

/// Persist the plugin selection for one durable Session.
pub async fn write_session_plugin_package_profile(
    directory: &str,
    server_id: &ServerId,
    session_path: &str,
    package_paths: &[String],
) -> pi_js::Result<()> {
    let normalized = normalize_plugin_package_paths(package_paths)?;
    let profile_path = session_plugin_profile_path(directory, server_id, session_path);
    write_plugin_package_profile(&profile_path, &normalized, Some(session_path)).await
}

/// Create a serialized server-owned builder for one configured plugin package.
pub fn create_server_plugin_package(
    directory: &str,
    server_id: &ServerId,
    package_path: &str,
) -> ConfiguredServerPluginPackage {
    let normalized_package_path = resolve_path(package_path, None, PathInputOptions::default());
    let outdir = join(&[
        directory,
        "plugin-builds",
        server_id,
        &plugin_build_directory_name(&normalized_package_path),
    ]);
    let manifest_path = join(&[&outdir, FACET_BUNDLE_MANIFEST_FILE]);
    ConfiguredServerPluginPackage {
        manifest_path,
        package_path: normalized_package_path,
        outdir,
        build_lock: Arc::new(AsyncMutex::new(())),
    }
}

async fn read_plugin_package_profile(
    path: &str,
    allow_empty: bool,
    session_path: Option<&str>,
) -> pi_js::Result<Option<Vec<String>>> {
    let raw = match read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.code() == Some("ENOENT") => return Ok(None),
        Err(error) => {
            return Err(pi_js::Error::js_with_cause(
                "Error",
                format!("Could not read experimental plugin package profile {path}"),
                error,
            ));
        }
    };
    let parsed = parse(&raw).map_err(|error| {
        pi_js::Error::js_with_cause(
            "Error",
            format!("Could not read experimental plugin package profile {path}"),
            error,
        )
    })?;
    let Some(obj) = parsed.as_object() else {
        return Err(pi_js::Error::msg(format!(
            "Invalid experimental plugin package profile {path}"
        )));
    };
    let invalid_keys = obj
        .keys()
        .any(|key| key != "version" && key != "packagePaths" && !(key == "sessionPath" && session_path.is_some()));
    let version_ok = obj.get("version").and_then(Value::as_i64) == Some(PLUGIN_PACKAGE_PROFILE_VERSION);
    let session_ok = match session_path {
        None => !obj.contains_key("sessionPath"),
        Some(expected) => obj.get("sessionPath").and_then(Value::as_str) == Some(expected),
    };
    let paths_arr = obj.get("packagePaths").and_then(Value::as_array);
    let paths_ok = paths_arr.is_some_and(|arr| {
        (allow_empty || !arr.is_empty()) && arr.iter().all(|item| item.as_str().is_some_and(|s| !s.is_empty()))
    });
    if invalid_keys || !version_ok || !session_ok || !paths_ok {
        return Err(pi_js::Error::msg(format!(
            "Invalid experimental plugin package profile {path}"
        )));
    }
    let Some(arr) = paths_arr else {
        return Err(pi_js::Error::msg(format!(
            "Invalid experimental plugin package profile {path}"
        )));
    };
    let raw_paths: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(ToString::to_string)).collect();
    Ok(Some(normalize_plugin_package_paths(&raw_paths)?))
}

async fn write_plugin_package_profile(
    path: &str,
    package_paths: &[String],
    session_path: Option<&str>,
) -> pi_js::Result<()> {
    let mut obj = Map::new();
    obj.insert("version".to_string(), Value::from(PLUGIN_PACKAGE_PROFILE_VERSION));
    if let Some(sp) = session_path {
        obj.insert("sessionPath".to_string(), Value::String(sp.to_string()));
    }
    obj.insert(
        "packagePaths".to_string(),
        Value::Array(package_paths.iter().cloned().map(Value::String).collect()),
    );
    let content = format!("{}\n", stringify_pretty(&Value::Object(obj), "  "));
    write_file(path, content.as_bytes(), Some(0o600)).await
}

pub fn normalize_plugin_package_paths(package_paths: &[String]) -> pi_js::Result<Vec<String>> {
    let mut normalized = Vec::with_capacity(package_paths.len());
    let mut seen = IndexSet::with_capacity(package_paths.len());
    for package_path in package_paths {
        if package_path.is_empty() {
            return Err(pi_js::Error::msg("Plugin package path must not be empty"));
        }
        let resolved = resolve_path(package_path, None, PathInputOptions::default());
        if !seen.insert(resolved.clone()) {
            return Err(pi_js::Error::msg("Plugin package paths must be unique"));
        }
        normalized.push(resolved);
    }
    Ok(normalized)
}

fn session_plugin_profile_path(directory: &str, server_id: &ServerId, session_path: &str) -> String {
    let hash = sha256_hex(session_path.as_bytes());
    let short_hash = &hash[..24.min(hash.len())];
    join(&[
        directory,
        &format!("session-plugin-packages-{server_id}-{short_hash}.json"),
    ])
}

fn plugin_build_directory_name(package_path: &str) -> String {
    let package_directory = if basename(package_path, None) == "package.json" {
        dirname(package_path)
    } else {
        package_path.to_string()
    };
    let raw_base = basename(&package_directory, None);
    let sanitized: String = raw_base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let base = if sanitized.is_empty() {
        "plugin".to_string()
    } else {
        sanitized
    };
    let label = if base.ends_with("-plugin") {
        base
    } else {
        format!("{base}-plugin")
    };
    let hash = sha256_hex(package_path.as_bytes());
    let short_hash = &hash[..12.min(hash.len())];
    format!("{label}-{short_hash}")
}

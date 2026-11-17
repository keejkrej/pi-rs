//! Port of packages/coding-agent/src/core/source-info.ts

use serde::{Deserialize, Serialize};

use crate::utils::paths::{canonicalize_path, get_path_comparison_key};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceScope {
    User,
    Project,
    Temporary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceOrigin {
    #[serde(rename = "package")]
    Package,
    #[serde(rename = "top-level")]
    TopLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMetadata {
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePackageIdentity {
    pub source: String,
    pub scope: SourceScope,
}

pub fn get_source_package_key(identity: &SourcePackageIdentity) -> String {
    pi_js::json::stringify(identity)
}

pub fn get_resource_path_key(path: &str) -> String {
    if is_synthetic_path(path) {
        path.to_owned()
    } else {
        get_path_comparison_key(&canonicalize_path(path))
    }
}

/// Prefix of built-in tool and extension paths, such as `builtin:read` or `builtin:mcp`.
pub const BUILTIN_PATH_PREFIX: &str = "builtin:";

/// Source of a path that names no file: `builtin` for `builtin:<name>`, or the prefix of an
/// angle-bracket path such as `inline` for `<inline:name>`. `None` for file paths.
pub fn get_synthetic_path_source(path: &str) -> Option<String> {
    if path.starts_with(BUILTIN_PATH_PREFIX) {
        return Some("builtin".to_owned());
    }
    if path.starts_with('<') && path.ends_with('>') && path.len() >= 2 {
        let inner = &path[1..path.len() - 1];
        let prefix = inner.split(':').next().unwrap_or("");
        return Some(if prefix.is_empty() {
            "temporary".to_owned()
        } else {
            prefix.to_owned()
        });
    }
    None
}

pub fn is_synthetic_path(path: &str) -> bool {
    path.starts_with(BUILTIN_PATH_PREFIX) || path.starts_with('<')
}

pub fn create_source_info(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_owned(),
        source: metadata.source.clone(),
        scope: metadata.scope,
        origin: metadata.origin,
        base_dir: metadata.base_dir.clone(),
        changelog_path: metadata.changelog_path.clone(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyntheticSourceOptions {
    pub source: String,
    pub scope: Option<SourceScope>,
    pub origin: Option<SourceOrigin>,
    pub base_dir: Option<String>,
    pub changelog_path: Option<String>,
}

pub fn create_synthetic_source_info(path: &str, options: SyntheticSourceOptions) -> SourceInfo {
    SourceInfo {
        path: path.to_owned(),
        source: options.source,
        scope: options.scope.unwrap_or(SourceScope::Temporary),
        origin: options.origin.unwrap_or(SourceOrigin::TopLevel),
        base_dir: options.base_dir,
        changelog_path: options.changelog_path,
    }
}

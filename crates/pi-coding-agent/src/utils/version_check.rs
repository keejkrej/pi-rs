//! Port of packages/coding-agent/src/utils/version-check.ts

use std::cmp::Ordering;

use node_semver::Version;
use pi_js::fetch::{Request, fetch};
use pi_js::{Error, Result};
use serde::Deserialize;

use crate::utils::management_http::{FetchRetryOptions, extract_error_cause_chain, fetch_with_retry};
use crate::utils::pi_user_agent::get_pi_user_agent;

const LATEST_VERSION_URL: &str = "https://pi.dev/api/latest-version";
const DEFAULT_VERSION_CHECK_TIMEOUT_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatestPiRelease {
    pub version: String,
    pub package_name: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct VersionCheckOptions {
    pub timeout_ms: Option<u64>,
    pub retry: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestReleaseResponse {
    package_name: Option<serde_json::Value>,
    version: Option<serde_json::Value>,
    note: Option<serde_json::Value>,
}

/// Include useful errno details hidden behind Node's generic "fetch failed" error.
pub fn format_version_check_error(error: &Error) -> String {
    let root_message = error.message();
    let root_msg = if root_message.is_empty() {
        error.to_js_string()
    } else {
        root_message
    };
    let chain = extract_error_cause_chain(error);
    if chain.len() > 1 {
        format!("{root_msg} (cause: {})", chain[1])
    } else {
        root_msg
    }
}

fn parse_valid_semver(raw: &str) -> Option<Version> {
    let trimmed = pi_js::str16::trim(raw);
    let cleaned = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('='))
        .unwrap_or(trimmed);
    Version::parse(cleaned).ok()
}

pub fn compare_package_versions(left_version: &str, right_version: &str) -> Option<i32> {
    let left = parse_valid_semver(left_version)?;
    let right = parse_valid_semver(right_version)?;
    Some(match left.cmp(&right) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    })
}

pub fn is_newer_package_version(candidate_version: &str, current_version: &str) -> bool {
    if let Some(comparison) = compare_package_versions(candidate_version, current_version) {
        return comparison > 0;
    }
    pi_js::str16::trim(candidate_version) != pi_js::str16::trim(current_version)
}

pub async fn get_latest_pi_release(
    current_version: &str,
    options: VersionCheckOptions,
) -> Result<Option<LatestPiRelease>> {
    if pi_js::env::var("PI_OFFLINE").is_some_and(|v| !v.is_empty()) {
        return Ok(None);
    }

    let req = Request::get(LATEST_VERSION_URL)
        .header("User-Agent", &get_pi_user_agent(current_version))
        .header("accept", "application/json");
    let response = if let Some(timeout) = options.timeout_ms {
        let mut opts = FetchRetryOptions::default();
        opts.max_retries = Some(if options.retry { 2.0 } else { 0.0 });
        opts.timeout_ms = Some(timeout);
        fetch_with_retry(req, opts).await?
    } else {
        let mut opts = FetchRetryOptions::default();
        opts.max_retries = Some(if options.retry { 2.0 } else { 0.0 });
        opts.timeout_ms = Some(DEFAULT_VERSION_CHECK_TIMEOUT_MS);
        let _ = fetch;
        fetch_with_retry(req, opts).await?
    };

    if !response.ok() {
        return Ok(None);
    }

    let json_val = response.json().await?;
    let data: LatestReleaseResponse = serde_json::from_value(json_val).map_err(|err| Error::msg(err.to_string()))?;
    let Some(version_str) = data
        .version
        .as_ref()
        .and_then(|v| v.as_str())
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };

    let package_name = data
        .package_name
        .as_ref()
        .and_then(|v| v.as_str())
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let note = data
        .note
        .as_ref()
        .and_then(|v| v.as_str())
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Ok(Some(LatestPiRelease {
        version: version_str.to_owned(),
        package_name,
        note,
    }))
}

pub async fn get_latest_pi_version(current_version: &str, options: VersionCheckOptions) -> Result<Option<String>> {
    Ok(get_latest_pi_release(current_version, options)
        .await?
        .map(|r| r.version))
}

pub async fn check_for_new_pi_version(current_version: &str) -> Option<LatestPiRelease> {
    if pi_js::env::var("PI_SKIP_VERSION_CHECK").is_some_and(|v| !v.is_empty()) {
        return None;
    }

    let release = get_latest_pi_release(current_version, VersionCheckOptions::default())
        .await
        .ok()??;
    if is_newer_package_version(&release.version, current_version) {
        Some(release)
    } else {
        None
    }
}

//! Port of packages/ai/src/utils/identity.ts

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use crate::utils::version::PI_AI_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StandardUserAgentMode {
    Short,
    Detailed,
}

/// The `User-Agent` mode sent to providers (`"short"`, `"detailed"`, or `{"custom": ...}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserAgentMode {
    Standard(StandardUserAgentMode),
    Custom { custom: String },
}

impl UserAgentMode {
    pub const SHORT: Self = Self::Standard(StandardUserAgentMode::Short);
    pub const DETAILED: Self = Self::Standard(StandardUserAgentMode::Detailed);

    pub fn custom(value: impl Into<String>) -> Self {
        Self::Custom { custom: value.into() }
    }
}

/// Who requests are attributed to. [`get_attribution_headers`] maps these fields to provider headers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientAttribution {
    /// App name: OpenRouter app title, NVIDIA invoke origin (first letter upper-cased), OpenCode client.
    pub name: String,
    /// App URL: OpenRouter `HTTP-Referer`, the app's identity in OpenRouter's rankings, grouped by origin.
    pub url: String,
    /// App categories, e.g. `["cli-agent"]`: OpenRouter `X-OpenRouter-Categories`, comma-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

/// Pi's built-in attribution, used by pi-agent-core, pi-durable, and the coding agent.
pub fn pi_attribution() -> ClientAttribution {
    ClientAttribution {
        name: "pi".to_owned(),
        url: "https://pi.dev".to_owned(),
        categories: Some(vec!["cli-agent".to_owned()]),
    }
}

fn deserialize_optional_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// How `pi-ai` identifies the calling app to providers. Every field is optional; absent fields use `pi-ai`'s defaults:
/// a detailed `User-Agent` with `pi-ai`'s version and no attribution headers.
///
/// PORT: `attribution` is `Option<Option<ClientAttribution>>` so `None` represents omitted (`undefined`) while
/// `Some(None)` represents explicit `null` (disabling attribution set by a lower-priority identity).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<UserAgentMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nullable"
    )]
    pub attribution: Option<Option<ClientAttribution>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The provider and endpoint a request goes to.
#[derive(Clone, Copy, Debug)]
pub struct ClientIdentityTarget<'a> {
    pub provider: &'a str,
    pub base_url: &'a str,
}

const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const OPENCODE_HOST: &str = "opencode.ai";

/// Merge identities field by field; later `Some` sources win and `None` fields are skipped.
pub fn merge_client_identity(sources: &[Option<&ClientIdentity>]) -> Option<ClientIdentity> {
    let mut merged: Option<ClientIdentity> = None;
    for source in sources.iter().flatten() {
        let entry = merged.get_or_insert_with(ClientIdentity::default);
        if let Some(ua) = &source.user_agent {
            entry.user_agent = Some(ua.clone());
        }
        if let Some(attr) = &source.attribution {
            entry.attribution = Some(attr.clone());
        }
        if let Some(version) = &source.version {
            entry.version = Some(version.clone());
        }
    }
    merged
}

/// Convenience helper for merging two optional [`ClientIdentity`] values where `override_identity` wins.
pub fn merge_two_client_identities(
    base: Option<&ClientIdentity>,
    override_identity: Option<&ClientIdentity>,
) -> Option<ClientIdentity> {
    merge_client_identity(&[base, override_identity])
}

/// The version in the detailed `User-Agent`.
pub fn get_client_version(identity: Option<&ClientIdentity>) -> &str {
    identity.and_then(|id| id.version.as_deref()).unwrap_or(PI_AI_VERSION)
}

/// The `User-Agent` value of an identity.
pub fn format_user_agent(identity: Option<&ClientIdentity>) -> String {
    match identity.and_then(|id| id.user_agent.as_ref()) {
        Some(UserAgentMode::Custom { custom }) => custom.clone(),
        Some(UserAgentMode::Standard(StandardUserAgentMode::Short)) => "pi".to_owned(),
        Some(UserAgentMode::Standard(StandardUserAgentMode::Detailed)) | None => {
            // PORT: Native Rust builds report `rust` as the runtime segment instead of `node/<ver>` or `bun/<ver>`.
            format!(
                "pi/{} ({}; rust; {})",
                get_client_version(identity),
                pi_js::env::platform(),
                pi_js::env::arch(),
            )
        }
    }
}

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == expected_host))
        .unwrap_or(false)
}

fn is_openrouter(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "openrouter" || target.base_url.contains("openrouter.ai")
}

fn is_nvidia_nim(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "nvidia" || matches_host(target.base_url, NVIDIA_NIM_HOST)
}

fn is_opencode(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "opencode" || target.provider == "opencode-go" || matches_host(target.base_url, OPENCODE_HOST)
}

/// Provider attribution headers of an identity for a request target. Empty when attribution is off.
pub fn get_attribution_headers(
    target: ClientIdentityTarget<'_>,
    identity: Option<&ClientIdentity>,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    let Some(attribution) = identity.and_then(|id| id.attribution.as_ref()).and_then(|a| a.as_ref()) else {
        return headers;
    };

    if is_openrouter(target) {
        headers.insert("HTTP-Referer".to_owned(), attribution.url.clone());
        headers.insert("X-OpenRouter-Title".to_owned(), attribution.name.clone());
        if let Some(categories) = &attribution.categories
            && !categories.is_empty()
        {
            headers.insert("X-OpenRouter-Categories".to_owned(), categories.join(","));
        }
        return headers;
    }

    // NVIDIA expects a capitalized origin, such as `Pi`.
    if is_nvidia_nim(target) {
        let mut chars = attribution.name.chars();
        let capitalized = match chars.next() {
            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
            None => String::new(),
        };
        headers.insert("X-BILLING-INVOKE-ORIGIN".to_owned(), capitalized);
        return headers;
    }

    if is_opencode(target) {
        headers.insert("x-opencode-client".to_owned(), attribution.name.clone());
        return headers;
    }

    headers
}

/// OpenCode's per-conversation routing header for requests to OpenCode, including custom providers pointing at it.
/// `None` without a session or for other targets.
pub fn get_opencode_session_headers(
    target: ClientIdentityTarget<'_>,
    session_id: Option<&str>,
) -> Option<IndexMap<String, String>> {
    let sid = session_id.filter(|s| !s.is_empty())?;
    if !is_opencode(target) {
        return None;
    }
    let mut headers = IndexMap::new();
    headers.insert("x-opencode-session".to_owned(), sid.to_owned());
    Some(headers)
}

/// Default headers an identity adds to a request: `User-Agent` and provider attribution.
pub fn get_identity_headers(
    target: ClientIdentityTarget<'_>,
    identity: Option<&ClientIdentity>,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert("User-Agent".to_owned(), format_user_agent(identity));
    for (k, v) in get_attribution_headers(target, identity) {
        headers.insert(k, v);
    }
    headers
}

/// Returns a [`crate::models::Models`] handle whose requests inherit `identity` as their default
/// client identity (a request's `identity` overrides it field by field, and it overrides the
/// identity `models` was created with).
pub fn with_client_identity(models: &crate::models::Models, identity: ClientIdentity) -> crate::models::Models {
    models.with_identity_override(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_and_turns_off_attribution_with_explicit_null() {
        let base = ClientIdentity {
            user_agent: Some(UserAgentMode::SHORT),
            attribution: Some(Some(pi_attribution())),
            version: Some("1.0.0".to_owned()),
        };
        let clear: ClientIdentity = serde_json::from_value(json!({ "attribution": null })).unwrap();
        assert_eq!(clear.attribution, Some(None));

        let merged = merge_two_client_identities(Some(&base), Some(&clear)).unwrap();
        assert_eq!(merged.user_agent, Some(UserAgentMode::SHORT));
        assert_eq!(merged.version.as_deref(), Some("1.0.0"));
        assert_eq!(merged.attribution, Some(None));

        let target = ClientIdentityTarget {
            provider: "openrouter",
            base_url: "https://openrouter.ai/api/v1",
        };
        assert!(get_attribution_headers(target, Some(&merged)).is_empty());
        assert!(!get_attribution_headers(target, Some(&base)).is_empty());
    }

    #[test]
    fn formats_user_agent_and_target_attribution_headers() {
        assert_eq!(
            format_user_agent(Some(&ClientIdentity {
                user_agent: Some(UserAgentMode::SHORT),
                ..Default::default()
            })),
            "pi"
        );
        assert_eq!(
            format_user_agent(Some(&ClientIdentity {
                user_agent: Some(UserAgentMode::custom("my-app/2.0")),
                ..Default::default()
            })),
            "my-app/2.0"
        );
        let detailed = format_user_agent(None);
        assert!(detailed.starts_with(&format!("pi/{PI_AI_VERSION} (")), "ua={detailed}");

        let id = ClientIdentity {
            attribution: Some(Some(pi_attribution())),
            ..Default::default()
        };
        let nvidia = ClientIdentityTarget {
            provider: "custom",
            base_url: "https://integrate.api.nvidia.com/v1",
        };
        assert_eq!(
            get_attribution_headers(nvidia, Some(&id))
                .get("X-BILLING-INVOKE-ORIGIN")
                .map(String::as_str),
            Some("Pi")
        );

        let opencode = ClientIdentityTarget {
            provider: "custom",
            base_url: "https://opencode.ai/zen/v1",
        };
        assert_eq!(
            get_attribution_headers(opencode, Some(&id))
                .get("x-opencode-client")
                .map(String::as_str),
            Some("pi")
        );
        assert_eq!(
            get_opencode_session_headers(opencode, Some("sess-1")).and_then(|h| h.get("x-opencode-session").cloned()),
            Some("sess-1".to_owned())
        );
    }
}

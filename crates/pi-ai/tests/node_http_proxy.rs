//! Port of packages/ai/test/node-http-proxy.test.ts (unit ai-utils-errors)

use pi_ai::types::ProviderEnv;
use pi_ai::utils::node_http_proxy::{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE, resolve_http_proxy_url_for_target};
use serial_test::serial;

const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "npm_config_http_proxy",
    "npm_config_https_proxy",
    "npm_config_proxy",
    "npm_config_no_proxy",
];

struct ProxyEnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl ProxyEnvGuard {
    fn new() -> Self {
        let saved = PROXY_ENV_KEYS.iter().map(|&k| (k, pi_js::env::var(k))).collect();
        for &k in PROXY_ENV_KEYS {
            pi_js::env::remove_var(k);
        }
        Self { saved }
    }
}

impl Drop for ProxyEnvGuard {
    fn drop(&mut self) {
        for &k in PROXY_ENV_KEYS {
            pi_js::env::remove_var(k);
        }
        for (k, v) in &self.saved {
            if let Some(val) = v {
                pi_js::env::set_var(k, val);
            }
        }
    }
}

#[test]
#[serial]
fn respects_no_proxy_exclusions() {
    let _guard = ProxyEnvGuard::new();
    pi_js::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");
    pi_js::env::set_var("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com");

    assert_eq!(
        resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", None).unwrap(),
        None
    );
}

#[test]
#[serial]
fn resolves_http_and_https_proxy_urls() {
    let _guard = ProxyEnvGuard::new();
    pi_js::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");

    let resolved = resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", None)
        .unwrap()
        .map(|u| u.to_string());
    assert_eq!(resolved.as_deref(), Some("http://proxy.example:8080/"));
}

#[test]
#[serial]
fn prefers_scoped_proxy_env_aliases_before_process_env_aliases() {
    let _guard = ProxyEnvGuard::new();
    pi_js::env::set_var("https_proxy", "http://process-proxy.example:8080");

    let mut scoped = ProviderEnv::new();
    scoped.insert(
        "HTTPS_PROXY".to_string(),
        "http://scoped-proxy.example:8080".to_string(),
    );

    let resolved = resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", Some(&scoped))
        .unwrap()
        .map(|u| u.to_string());
    assert_eq!(resolved.as_deref(), Some("http://scoped-proxy.example:8080/"));
}

#[test]
#[serial]
fn rejects_socks_and_pac_proxy_urls_explicitly() {
    let _guard = ProxyEnvGuard::new();
    pi_js::env::set_var("HTTPS_PROXY", "socks5://proxy.example:1080");

    let err = resolve_http_proxy_url_for_target("https://bedrock-runtime.us-east-1.amazonaws.com", None).unwrap_err();
    assert!(err.message().contains(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE));
}

#[test]
#[serial]
fn handles_subdomain_wildcards_ipv6_and_ports_in_no_proxy() {
    let _guard = ProxyEnvGuard::new();
    pi_js::env::set_var("HTTPS_PROXY", "http://proxy.example:8080");
    pi_js::env::set_var(
        "NO_PROXY",
        "example.com, .wildcard.org, *.star.net, ::1, [2001:db8::1], 127.0.0.1:8080",
    );

    assert_eq!(
        resolve_http_proxy_url_for_target("https://example.com", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://api.example.com", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://wildcard.org", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://api.wildcard.org", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://star.net", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://api.star.net", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://notexample.com", None)
            .unwrap()
            .map(|u| u.to_string())
            .as_deref(),
        Some("http://proxy.example:8080/")
    );

    assert_eq!(
        resolve_http_proxy_url_for_target("https://[::1]:80", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://[2001:db8::1]", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://127.0.0.1:8080", None).unwrap(),
        None
    );
    assert_eq!(
        resolve_http_proxy_url_for_target("https://127.0.0.1:3000", None)
            .unwrap()
            .map(|u| u.to_string())
            .as_deref(),
        Some("http://proxy.example:8080/")
    );
}

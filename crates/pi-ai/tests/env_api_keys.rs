//! Port of packages/ai/test/env-api-keys.test.ts (unit ai-providers-builtin)

use pi_ai::env_api_keys::{find_env_keys, get_env_api_key};
use serial_test::serial;

const ENV_KEYS: &[&str] = &[
    "COPILOT_GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "ZAI_CODING_CN_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new() -> Self {
        let saved = ENV_KEYS.iter().map(|&k| (k, pi_js::env::var(k))).collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => pi_js::env::set_var(k, val),
                None => pi_js::env::remove_var(k),
            }
        }
    }
}

#[test]
#[serial]
fn does_not_treat_generic_github_tokens_as_github_copilot_credentials() {
    let _guard = EnvGuard::new();
    pi_js::env::remove_var("COPILOT_GITHUB_TOKEN");
    pi_js::env::set_var("GH_TOKEN", "gh-token");
    pi_js::env::set_var("GITHUB_TOKEN", "github-token");

    assert_eq!(find_env_keys("github-copilot", None), None);
    assert_eq!(get_env_api_key("github-copilot", None), None);
}

#[test]
#[serial]
fn resolves_github_copilot_credentials_from_copilot_github_token() {
    let _guard = EnvGuard::new();
    pi_js::env::set_var("COPILOT_GITHUB_TOKEN", "copilot-token");
    pi_js::env::set_var("GH_TOKEN", "gh-token");
    pi_js::env::set_var("GITHUB_TOKEN", "github-token");

    assert_eq!(
        find_env_keys("github-copilot", None),
        Some(vec!["COPILOT_GITHUB_TOKEN"])
    );
    assert_eq!(
        get_env_api_key("github-copilot", None),
        Some("copilot-token".to_string())
    );
}

#[test]
#[serial]
fn resolves_zai_china_coding_plan_credentials_from_zai_coding_cn_api_key() {
    let _guard = EnvGuard::new();
    pi_js::env::set_var("ZAI_CODING_CN_API_KEY", "zai-coding-cn-token");

    assert_eq!(
        find_env_keys("zai-coding-cn", None),
        Some(vec!["ZAI_CODING_CN_API_KEY"])
    );
    assert_eq!(
        get_env_api_key("zai-coding-cn", None),
        Some("zai-coding-cn-token".to_string())
    );
}

#[test]
#[serial]
fn reports_anthropic_auth_token_but_preserves_oauth_token_api_key_lookup() {
    let _guard = EnvGuard::new();
    pi_js::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token");
    pi_js::env::set_var("ANTHROPIC_OAUTH_TOKEN", "oauth-token");
    pi_js::env::set_var("ANTHROPIC_API_KEY", "api-key");

    assert_eq!(
        find_env_keys("anthropic", None),
        Some(vec![
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ])
    );
    assert_eq!(get_env_api_key("anthropic", None), Some("oauth-token".to_string()));
}

#[test]
#[serial]
fn does_not_return_anthropic_auth_token_as_api_key() {
    let _guard = EnvGuard::new();
    pi_js::env::set_var("ANTHROPIC_AUTH_TOKEN", "auth-token");
    pi_js::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
    pi_js::env::remove_var("ANTHROPIC_API_KEY");

    assert_eq!(find_env_keys("anthropic", None), Some(vec!["ANTHROPIC_AUTH_TOKEN"]));
    assert_eq!(get_env_api_key("anthropic", None), None);
}

#[test]
#[serial]
fn preserves_anthropic_oauth_token_as_api_key() {
    let _guard = EnvGuard::new();
    pi_js::env::remove_var("ANTHROPIC_AUTH_TOKEN");
    pi_js::env::set_var("ANTHROPIC_OAUTH_TOKEN", "oauth-token");
    pi_js::env::remove_var("ANTHROPIC_API_KEY");

    assert_eq!(find_env_keys("anthropic", None), Some(vec!["ANTHROPIC_OAUTH_TOKEN"]));
    assert_eq!(get_env_api_key("anthropic", None), Some("oauth-token".to_string()));
}

#[test]
#[serial]
fn falls_back_to_anthropic_api_key_for_api_key_lookup() {
    let _guard = EnvGuard::new();
    pi_js::env::remove_var("ANTHROPIC_AUTH_TOKEN");
    pi_js::env::remove_var("ANTHROPIC_OAUTH_TOKEN");
    pi_js::env::set_var("ANTHROPIC_API_KEY", "api-key");

    assert_eq!(get_env_api_key("anthropic", None), Some("api-key".to_string()));
}

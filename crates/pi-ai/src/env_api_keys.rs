//! Port of packages/ai/src/env-api-keys.ts

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const ANTHROPIC_FEDERATION_RULE_ID_ENV: &str = "ANTHROPIC_FEDERATION_RULE_ID";
pub const ANTHROPIC_ORGANIZATION_ID_ENV: &str = "ANTHROPIC_ORGANIZATION_ID";
pub const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV: &str = "ANTHROPIC_SERVICE_ACCOUNT_ID";
pub const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV: &str = "ANTHROPIC_IDENTITY_TOKEN_FILE";
pub const ANTHROPIC_WORKSPACE_ID_ENV: &str = "ANTHROPIC_WORKSPACE_ID";

fn has_vertex_adc_credentials(env: Option<&ProviderEnv>) -> bool {
    if let Some(explicit_path) = env
        .and_then(|map| map.get("GOOGLE_APPLICATION_CREDENTIALS"))
        .filter(|s| !s.is_empty())
    {
        return pi_js::fs::exists(explicit_path);
    }

    if let Some(gac_path) = get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env) {
        return pi_js::fs::exists(&gac_path);
    }

    let home = pi_js::env::home_dir();
    if home.is_empty() {
        return false;
    }
    let default_adc = std::path::PathBuf::from(home)
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");
    pi_js::fs::exists(&default_adc.to_string_lossy())
}

const ANTHROPIC_ENV_VARS: &[&str] = &[
    ANTHROPIC_AUTH_TOKEN_ENV,
    ANTHROPIC_OAUTH_TOKEN_ENV,
    ANTHROPIC_API_KEY_ENV,
];

fn get_api_key_env_vars(provider: &str) -> Option<&'static [&'static str]> {
    if provider == "github-copilot" {
        return Some(&["COPILOT_GITHUB_TOKEN"]);
    }

    // ANTHROPIC_AUTH_TOKEN participates in env discovery/status, but
    // get_env_api_key() skips it because requests must pass it as Authorization: Bearer.
    if provider == "anthropic" {
        return Some(ANTHROPIC_ENV_VARS);
    }

    match provider {
        "ant-ling" => Some(&["ANT_LING_API_KEY"]),
        "qwen-token-plan" => Some(&["QWEN_TOKEN_PLAN_API_KEY"]),
        "qwen-token-plan-cn" => Some(&["QWEN_TOKEN_PLAN_CN_API_KEY"]),
        "qwen-token-plan-individual" => Some(&["QWEN_TOKEN_PLAN_API_KEY"]),
        "openai" => Some(&["OPENAI_API_KEY"]),
        "azure-openai-responses" => Some(&["AZURE_OPENAI_API_KEY"]),
        "nvidia" => Some(&["NVIDIA_API_KEY"]),
        "deepseek" => Some(&["DEEPSEEK_API_KEY"]),
        "google" => Some(&["GEMINI_API_KEY"]),
        "google-vertex" => Some(&["GOOGLE_CLOUD_API_KEY"]),
        "groq" => Some(&["GROQ_API_KEY"]),
        "cerebras" => Some(&["CEREBRAS_API_KEY"]),
        "xai" => Some(&["XAI_API_KEY"]),
        "typesafe" => Some(&["TYPESAFE_API_KEY"]),
        "radius" => Some(&["RADIUS_API_KEY"]),
        "openrouter" => Some(&["OPENROUTER_API_KEY"]),
        "vercel-ai-gateway" => Some(&["AI_GATEWAY_API_KEY"]),
        "zai" => Some(&["ZAI_API_KEY"]),
        "zai-coding-cn" => Some(&["ZAI_CODING_CN_API_KEY"]),
        "mistral" => Some(&["MISTRAL_API_KEY"]),
        "minimax" => Some(&["MINIMAX_API_KEY"]),
        "minimax-cn" => Some(&["MINIMAX_CN_API_KEY"]),
        "moonshotai" => Some(&["MOONSHOT_API_KEY"]),
        "moonshotai-cn" => Some(&["MOONSHOT_API_KEY"]),
        "huggingface" => Some(&["HF_TOKEN"]),
        "fireworks" => Some(&["FIREWORKS_API_KEY"]),
        "together" => Some(&["TOGETHER_API_KEY"]),
        "baseten" => Some(&["BASETEN_API_KEY"]),
        "opencode" => Some(&["OPENCODE_API_KEY"]),
        "opencode-go" => Some(&["OPENCODE_API_KEY"]),
        "kimi-coding" => Some(&["KIMI_API_KEY"]),
        "meta" => Some(&["META_API_KEY"]),
        "cloudflare-workers-ai" => Some(&["CLOUDFLARE_API_KEY"]),
        "cloudflare-ai-gateway" => Some(&["CLOUDFLARE_API_KEY"]),
        "xiaomi" => Some(&["XIAOMI_API_KEY"]),
        "xiaomi-token-plan-cn" => Some(&["XIAOMI_TOKEN_PLAN_CN_API_KEY"]),
        "xiaomi-token-plan-ams" => Some(&["XIAOMI_TOKEN_PLAN_AMS_API_KEY"]),
        "xiaomi-token-plan-sgp" => Some(&["XIAOMI_TOKEN_PLAN_SGP_API_KEY"]),
        _ => None,
    }
}

/// Find configured environment variables that can provide an API key for a provider.
///
/// This only reports actual API key variables and excludes ambient credential sources
/// such as AWS profiles, AWS IAM credentials, and Google Application Default Credentials.
pub fn find_env_keys(provider: impl AsRef<str>, env: Option<&ProviderEnv>) -> Option<Vec<&'static str>> {
    let env_vars = get_api_key_env_vars(provider.as_ref())?;
    let found: Vec<&'static str> = env_vars
        .iter()
        .copied()
        .filter(|env_var| get_provider_env_value(env_var, env).is_some())
        .collect();
    if found.is_empty() { None } else { Some(found) }
}

/// Get API key for provider from known environment variables, e.g. `OPENAI_API_KEY`.
pub fn get_env_api_key(provider: impl AsRef<str>, env: Option<&ProviderEnv>) -> Option<String> {
    let provider_id = provider.as_ref();
    if let Some(env_keys) = find_env_keys(provider_id, env) {
        let api_key_env = if provider_id == "anthropic" {
            env_keys.iter().copied().find(|k| *k != ANTHROPIC_AUTH_TOKEN_ENV)
        } else {
            env_keys.first().copied()
        };
        if let Some(var_name) = api_key_env {
            return get_provider_env_value(var_name, env);
        }
    }

    // Vertex AI supports either an explicit API key or Application Default Credentials.
    if provider_id == "google-vertex" {
        let has_credentials = has_vertex_adc_credentials(env);
        let has_project = get_provider_env_value("GOOGLE_CLOUD_PROJECT", env).is_some()
            || get_provider_env_value("GCLOUD_PROJECT", env).is_some();
        let has_location = get_provider_env_value("GOOGLE_CLOUD_LOCATION", env).is_some();

        if has_credentials && has_project && has_location {
            return Some("<authenticated>".to_owned());
        }
    }

    if provider_id == "amazon-bedrock" {
        let has_iam_keys = get_provider_env_value("AWS_ACCESS_KEY_ID", env).is_some()
            && get_provider_env_value("AWS_SECRET_ACCESS_KEY", env).is_some();
        if get_provider_env_value("AWS_PROFILE", env).is_some()
            || has_iam_keys
            || get_provider_env_value("AWS_BEARER_TOKEN_BEDROCK", env).is_some()
            || get_provider_env_value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", env).is_some()
            || get_provider_env_value("AWS_CONTAINER_CREDENTIALS_FULL_URI", env).is_some()
            || get_provider_env_value("AWS_WEB_IDENTITY_TOKEN_FILE", env).is_some()
        {
            return Some("<authenticated>".to_owned());
        }
    }

    None
}

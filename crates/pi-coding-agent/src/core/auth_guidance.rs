//! Port of packages/coding-agent/src/core/auth-guidance.ts

use crate::config::get_docs_path;

const UNKNOWN_PROVIDER: &str = "unknown";

pub fn get_provider_login_help() -> String {
    let docs = get_docs_path();
    [
        "Use /login to log into a provider via OAuth or API key. See:".to_owned(),
        format!("  {}", pi_js::path::join(&[&docs, "providers.md"])),
        format!("  {}", pi_js::path::join(&[&docs, "models.md"])),
    ]
    .join("\n")
}

pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

pub fn format_no_model_selected_message() -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        get_provider_login_help()
    )
}

pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {provider_display}.\n\n{}",
        get_provider_login_help()
    )
}

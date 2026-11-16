//! Port of packages/ai/src/auth/context.ts

use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::types::AuthContext;

/// Default auth context: env vars from process environment and filesystem existence checks.
#[derive(Clone, Debug, Default)]
pub struct DefaultProviderAuthContext;

#[async_trait::async_trait]
impl AuthContext for DefaultProviderAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        pi_js::env::var(name).filter(|v| !v.trim().is_empty())
    }

    async fn file_exists(&self, path: &str) -> bool {
        let resolved = if let Some(rest) = path.strip_prefix("~/") {
            let home = pi_js::env::home_dir();
            if home.is_empty() {
                return false;
            }
            PathBuf::from(home).join(rest)
        } else if path == "~" {
            let home = pi_js::env::home_dir();
            if home.is_empty() {
                return false;
            }
            PathBuf::from(home)
        } else if let Some(rest) = path.strip_prefix('~') {
            let home = pi_js::env::home_dir();
            if home.is_empty() {
                return false;
            }
            PathBuf::from(format!("{home}{rest}"))
        } else {
            PathBuf::from(path)
        };
        pi_js::fs::exists(&resolved.to_string_lossy())
    }
}

pub fn default_provider_auth_context() -> Arc<dyn AuthContext> {
    Arc::new(DefaultProviderAuthContext)
}

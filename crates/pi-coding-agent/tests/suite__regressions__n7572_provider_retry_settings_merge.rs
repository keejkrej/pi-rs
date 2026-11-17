//! Port of packages/coding-agent/test/suite/regressions/7572-provider-retry-settings-merge.test.ts

use pi_coding_agent::core::settings_manager::{
    InMemorySettingsStorage, ResolvedProviderRetrySettings, SettingsManager, SettingsScope,
};
use serde_json::json;

#[test]
fn preserves_global_provider_settings_not_overridden_by_the_project() {
    let storage = InMemorySettingsStorage::new();
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "retry": {
                    "provider": {
                        "timeoutMs": 30000,
                        "maxRetryDelayMs": 45000
                    }
                }
            }))))
        })
        .expect("write global settings");
    storage
        .with_lock(SettingsScope::Project, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "retry": {
                    "provider": {
                        "maxRetries": 2
                    }
                }
            }))))
        })
        .expect("write project settings");

    let settings_manager = SettingsManager::from_storage(std::sync::Arc::new(storage), None);

    assert_eq!(
        settings_manager.get_provider_retry_settings(),
        ResolvedProviderRetrySettings {
            timeout_ms: Some(30000),
            max_retries: Some(2),
            max_retry_delay_ms: 45000,
        }
    );
}

//! Port of packages/coding-agent/test/settings-manager-compaction.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_coding_agent::core::settings_manager::{
    CompactionSettings, InMemorySettingsStorage, ModelRef, ResolvedCompactionSettings, Settings, SettingsManager,
    SettingsScope,
};
use serde_json::{Value, json};

fn model() -> ModelRef {
    ModelRef {
        provider: "provider".to_owned(),
        id: "family/model".to_owned(),
    }
}

const MODEL_KEY: &str = "provider/family/model";

fn defaults() -> ResolvedCompactionSettings {
    ResolvedCompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens: 20_000,
    }
}

#[test]
fn uses_defaults_without_compaction_settings() {
    let manager = SettingsManager::in_memory(None, None);
    assert_eq!(manager.get_compaction_settings(None).unwrap(), defaults());
    assert_eq!(manager.get_compaction_settings(Some(&model())).unwrap(), defaults());
}

#[test]
fn resolves_each_field_independently_and_keeps_individual_getters_consistent() {
    let m = model();
    let manager = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: None,
                reserve_tokens: Some(8_192),
                keep_recent_tokens: Some(10_000),
                model_overrides: Some(IndexMap::from([(
                    MODEL_KEY.to_owned(),
                    json!({ "reserveTokens": 400_000 }),
                )])),
            }),
            ..Default::default()
        }),
        None,
    );

    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 400_000,
            keep_recent_tokens: 10_000,
        }
    );
    assert_eq!(manager.get_compaction_reserve_tokens(Some(&m)).unwrap(), 400_000);
    assert_eq!(manager.get_compaction_keep_recent_tokens(Some(&m)).unwrap(), 10_000);
    assert_eq!(
        manager.get_compaction_settings(None).unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 8_192,
            keep_recent_tokens: 10_000,
        }
    );

    manager.apply_overrides(&Settings {
        compaction: Some(CompactionSettings {
            model_overrides: Some(IndexMap::from([(
                MODEL_KEY.to_owned(),
                json!({ "keepRecentTokens": 30_000 }),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert_eq!(manager.get_compaction_keep_recent_tokens(Some(&m)).unwrap(), 30_000);
    assert_eq!(manager.get_compaction_reserve_tokens(Some(&m)).unwrap(), 400_000);
}

#[test]
fn falls_back_to_built_in_defaults_for_missing_fields() {
    let m = model();
    let manager = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                model_overrides: Some(IndexMap::from([(
                    MODEL_KEY.to_owned(),
                    json!({ "keepRecentTokens": 1_024 }),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );
    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            keep_recent_tokens: 1_024,
            ..defaults()
        }
    );
}

#[test]
fn matches_exact_provider_model_ids_including_ids_containing_slashes() {
    let m = model();
    let manager = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                model_overrides: Some(IndexMap::from([
                    (MODEL_KEY.to_owned(), json!({ "reserveTokens": 400_000 })),
                    ("provider/*".to_owned(), json!({ "reserveTokens": 1 })),
                    ("family/model".to_owned(), json!({ "reserveTokens": 2 })),
                ])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );
    assert_eq!(manager.get_compaction_reserve_tokens(Some(&m)).unwrap(), 400_000);

    for other in [
        ModelRef {
            provider: "other".to_owned(),
            id: m.id.clone(),
        },
        ModelRef {
            provider: m.provider.clone(),
            id: "other".to_owned(),
        },
        ModelRef {
            provider: m.provider.clone(),
            id: "family/Model".to_owned(),
        },
    ] {
        assert_eq!(manager.get_compaction_settings(Some(&other)).unwrap(), defaults());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn merges_project_model_overrides_per_field_before_resolving_fallbacks() {
    let m = model();
    let storage = Arc::new(InMemorySettingsStorage::new());
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "compaction": {
                    "reserveTokens": 8192,
                    "modelOverrides": {
                        MODEL_KEY: { "reserveTokens": 400000, "keepRecentTokens": 30000 },
                        "provider/other": { "keepRecentTokens": 4096 }
                    }
                }
            }))))
        })
        .unwrap();
    storage
        .with_lock(SettingsScope::Project, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "compaction": {
                    "reserveTokens": 1024,
                    "modelOverrides": { MODEL_KEY: { "keepRecentTokens": 2000 } }
                }
            }))))
        })
        .unwrap();

    let manager = SettingsManager::from_storage(storage, None);
    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 400_000,
            keep_recent_tokens: 2_000,
        }
    );
    assert_eq!(
        manager
            .get_compaction_settings(Some(&ModelRef {
                provider: "provider".to_owned(),
                id: "other".to_owned()
            }))
            .unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 1_024,
            keep_recent_tokens: 4_096,
        }
    );

    manager.reload().await;
    assert_eq!(manager.get_compaction_keep_recent_tokens(Some(&m)).unwrap(), 2_000);
    manager.set_project_trusted(false);
    assert_eq!(manager.get_compaction_keep_recent_tokens(Some(&m)).unwrap(), 30_000);
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_enabled_global_and_preserves_overrides_when_saving_the_toggle() {
    let m = model();
    let storage = Arc::new(InMemorySettingsStorage::new());
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "compaction": {
                    "modelOverrides": { MODEL_KEY: { "enabled": false, "reserveTokens": 400000 } }
                }
            }))))
        })
        .unwrap();

    let manager = SettingsManager::from_storage(storage, None);
    assert!(manager.get_compaction_settings(Some(&m)).unwrap().enabled);
    manager.set_compaction_enabled(false);
    manager.flush().await;
    manager.reload().await;
    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            enabled: false,
            reserve_tokens: 400_000,
            keep_recent_tokens: defaults().keep_recent_tokens,
        }
    );
}

#[test]
fn reports_invalid_model_override_token_values() {
    let m = model();
    let other_m = ModelRef {
        provider: "other".to_owned(),
        id: m.id.clone(),
    };

    // PORT: Rust `get_compaction_token_setting` treats `null` as unset (`!raw_override.is_null()`),
    // and non-finite JS values (`NaN`/`Infinity`) are not representable in `serde_json::Value`.
    for field in ["reserveTokens", "keepRecentTokens"] {
        for value in [
            json!(-1),
            json!(1.5),
            json!("400000"),
            json!(true),
            json!({}),
            json!([]),
            json!(9_007_199_254_740_992_u64),
        ] {
            let storage = Arc::new(InMemorySettingsStorage::new());
            let body = json!({
                "compaction": {
                    "modelOverrides": { MODEL_KEY: { field: value } }
                }
            });
            storage
                .with_lock(SettingsScope::Global, |_| Ok(Some(pi_js::json::stringify(&body))))
                .unwrap();

            let manager = SettingsManager::from_storage(storage, None);
            let err = manager
                .get_compaction_settings(Some(&m))
                .expect_err("expected invalid override token value");
            assert!(
                err.to_string().contains(&format!(
                    "Invalid compaction.modelOverrides[\"{MODEL_KEY}\"].{field} setting:"
                )),
                "unexpected err: {err}"
            );
            assert!(err.to_string().contains("Expected a non-negative safe integer."));
            assert_eq!(manager.get_compaction_settings(None).unwrap(), defaults());
            assert_eq!(manager.get_compaction_settings(Some(&other_m)).unwrap(), defaults());
        }
    }
}

#[test]
fn reports_invalid_ordinary_compaction_token_values() {
    let m = model();
    // PORT: In Rust `CompactionSettings.{reserve_tokens,keep_recent_tokens}` are typed as `Option<i64>`,
    // so negative integer values pass deserialization and are validated at read time even with a valid override.
    for (field, json_body) in [
        (
            "reserveTokens",
            json!({
                "compaction": {
                    "reserveTokens": -1,
                    "modelOverrides": { MODEL_KEY: { "reserveTokens": 4096 } }
                }
            }),
        ),
        (
            "keepRecentTokens",
            json!({
                "compaction": {
                    "keepRecentTokens": -1,
                    "modelOverrides": { MODEL_KEY: { "keepRecentTokens": 4096 } }
                }
            }),
        ),
    ] {
        let storage = Arc::new(InMemorySettingsStorage::new());
        storage
            .with_lock(SettingsScope::Global, |_| Ok(Some(pi_js::json::stringify(&json_body))))
            .unwrap();
        let manager = SettingsManager::from_storage(storage, None);
        let expected = format!("Invalid compaction.{field} setting: -1. Expected a non-negative safe integer.");
        assert_eq!(
            manager.get_compaction_settings(None).expect_err("ordinary").to_string(),
            expected
        );
        assert_eq!(
            manager
                .get_compaction_settings(Some(&m))
                .expect_err("with override")
                .to_string(),
            expected
        );
    }
}

#[test]
fn reports_malformed_model_entries() {
    let m = model();
    for entry in [Value::Null, json!(false), json!(42), json!("invalid"), json!([])] {
        let storage = Arc::new(InMemorySettingsStorage::new());
        let body = json!({
            "compaction": {
                "modelOverrides": { MODEL_KEY: entry }
            }
        });
        storage
            .with_lock(SettingsScope::Global, |_| Ok(Some(pi_js::json::stringify(&body))))
            .unwrap();
        let err = SettingsManager::from_storage(storage, None)
            .get_compaction_settings(Some(&m))
            .expect_err("malformed entry");
        assert!(
            err.to_string()
                .contains(&format!("Invalid compaction.modelOverrides[\"{MODEL_KEY}\"] setting:")),
            "unexpected err: {err}"
        );
        assert!(err.to_string().contains("Expected an object."));
    }
}

#[test]
fn accepts_zero_in_ordinary_settings_and_model_overrides() {
    let m = model();
    let manager = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                reserve_tokens: Some(0),
                keep_recent_tokens: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );
    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 0,
        }
    );

    manager.apply_overrides(&Settings {
        compaction: Some(CompactionSettings {
            reserve_tokens: Some(1_000),
            keep_recent_tokens: Some(1_000),
            model_overrides: Some(IndexMap::from([(
                MODEL_KEY.to_owned(),
                json!({ "reserveTokens": 0, "keepRecentTokens": 0 }),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert_eq!(
        manager.get_compaction_settings(Some(&m)).unwrap(),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 0,
        }
    );
}

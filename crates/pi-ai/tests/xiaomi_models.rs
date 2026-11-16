//! Port of packages/ai/test/xiaomi-models.test.ts

use pi_ai::compat::get_models;

const XIAOMI_PROVIDERS: &[&str] = &[
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
];
const DEPRECATED_MODEL_IDS: &[&str] = &["mimo-v2-flash", "mimo-v2-omni", "mimo-v2-pro"];
const REPLACEMENT_MODEL_IDS: &[&str] = &["mimo-v2.5", "mimo-v2.5-pro"];

#[test]
fn omits_deprecated_models_from_xiaomi_providers() {
    for &provider in XIAOMI_PROVIDERS {
        let model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        for &deprecated in DEPRECATED_MODEL_IDS {
            assert!(
                !model_ids.iter().any(|id| id == deprecated),
                "{provider} should omit {deprecated}"
            );
        }
    }
}

#[test]
fn keeps_replacement_models_on_xiaomi_providers() {
    for &provider in XIAOMI_PROVIDERS {
        let model_ids: Vec<String> = get_models(Some(provider)).into_iter().map(|m| m.id).collect();
        for &replacement in REPLACEMENT_MODEL_IDS {
            assert!(
                model_ids.iter().any(|id| id == replacement),
                "{provider} should keep {replacement}"
            );
        }
    }
}

//! Port of packages/coding-agent/test/config-schemas.test.ts

use std::path::Path;

use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::keybindings_schema::keybindings_schema;
use pi_coding_agent::core::model_config::{ModelConfig, models_config_schema};
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::settings_schema::settings_schema;
use pi_coding_agent::modes::interactive::theme::theme_schema::{theme_json_schema, validate_theme_json};
use pi_schema::{CompileOptions, compile};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

const SCHEMA_BASE_URL: &str = "https://pi.dev/schemas";

fn schema_url(name: &str) -> String {
    format!("{SCHEMA_BASE_URL}/{name}.schema.json")
}

fn read_package_schema(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("package/schemas")
        .join(format!("{name}.schema.json"));
    let text = std::fs::read_to_string(&path).expect("package schema readable");
    serde_json::from_str(&text).expect("valid package schema JSON")
}

#[test]
fn package_schemas_preserve_editor_guidance() {
    let theme_schema = read_package_schema("theme");
    assert_eq!(theme_schema["title"], "Pi Coding Agent Theme");
    assert_eq!(theme_schema["description"], "Theme schema for Pi coding agent");
    let colors = &theme_schema["properties"]["colors"];
    assert!(
        colors["description"]
            .as_str()
            .is_some_and(|s| s.contains("use compatible fallbacks"))
    );
    assert!(
        colors["properties"]["scrollbarTrack"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("falls back to muted"))
    );
    assert_eq!(
        colors["properties"]["accent"]["description"],
        "Primary accent color (logo, selected items, cursor)"
    );
    assert!(
        theme_schema["properties"]["export"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("defaults derived from userMessageBg"))
    );

    let models_schema = read_package_schema("models");
    assert!(
        models_schema["$defs"]["ModelCost"]["properties"]["input"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("USD per million tokens"))
    );
    assert!(
        models_schema["$defs"]["ModelCost"]["properties"]["tiers"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("highest matching input threshold"))
    );
    assert!(
        models_schema["$defs"]["ModelInputLimits"]["properties"]["maxRequestBytes"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("serialized provider request size"))
    );

    let settings = read_package_schema("settings");
    assert!(
        settings["properties"]["quietStartup"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("hide all startup output"))
    );
    assert!(
        settings["properties"]["defaultTools"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("+name and -name"))
    );
    assert!(
        settings["properties"]["fullscreenWheelScrollLines"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("1 to 100"))
    );
}

#[test]
fn generated_schemas_preserve_editor_guidance_and_legacy_migrations() {
    let theme_val = theme_json_schema().into_value();
    assert_eq!(theme_val["title"], "Pi Coding Agent Theme");
    assert_eq!(theme_val["description"], "Theme schema for Pi coding agent");
    assert!(
        theme_val["properties"]["colors"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("use compatible fallbacks"))
    );
    assert_eq!(
        theme_val["properties"]["colors"]["properties"]["accent"]["description"],
        "Primary accent color (logo, selected items, cursor)"
    );

    let settings_val = settings_schema().into_value();
    assert!(
        settings_val["properties"]["retry"]["properties"]["maxDelayMs"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("provider.maxRetryDelayMs"))
    );
    assert_eq!(
        settings_val["properties"]["queueMode"]["description"],
        "Legacy setting migrated to steeringMode."
    );
    assert!(
        settings_val["properties"]["quietStartup"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("hide all startup output"))
    );
    assert!(
        settings_val["properties"]["defaultTools"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("+name and -name"))
    );
    assert!(
        settings_val["properties"]["fullscreenWheelScrollLines"]["description"]
            .as_str()
            .is_some_and(|s| s.contains("1 to 100"))
    );
}

#[test]
fn validates_representative_config_documents() {
    let models_validator = compile(models_config_schema(), CompileOptions::default()).unwrap();
    let valid_models = json!({
        "providers": {
            "local": {
                "compat": {
                    "zaiToolStream": true,
                    "thinkingTokenBudgetField": "thinking_budget",
                    "chatTemplateKwargs": { "budget": { "$var": "thinking.budget" } },
                    "supportsMidConvoSystemMessages": true
                },
                "models": [
                    {
                        "id": "model",
                        "inputLimits": { "maxRequestBytes": 1024, "images": { "maxPerRequest": 2 } },
                        "promptCache": { "short": 300 }
                    }
                ]
            }
        }
    });
    assert!(models_validator.check(&valid_models));
    assert!(!models_validator.check(&json!({ "providers": { "local": { "models": [{ "id": "" }] } } })));

    let settings_validator = compile(settings_schema(), CompileOptions::default()).unwrap();
    let valid_settings = json!({
        "theme": "dark",
        "cacheWarming": "idle",
        "extensionSetting": { "enabled": true },
        "queueMode": "all",
        "websockets": true,
        "skills": { "enableSkillCommands": true, "customDirectories": ["./skills"] },
        "retry": { "maxDelayMs": 1000 }
    });
    assert!(settings_validator.check(&valid_settings));
    assert!(!settings_validator.check(&json!({ "cacheWarming": "always" })));

    let kb_validator = compile(keybindings_schema(), CompileOptions::default()).unwrap();
    assert!(kb_validator.check(&json!({ "app.session.new": "ctrl+n", "extension.action": ["alt+x"] })));
    assert!(!kb_validator.check(&json!({ "app.session.new": 42 })));
}

#[test]
fn rejects_non_positive_model_token_limits() {
    let validator = compile(models_config_schema(), CompileOptions::default()).unwrap();
    for field in ["contextWindow", "maxTokens"] {
        for value in [0, -1] {
            assert!(
                !validator.check(&json!({ "providers": { "local": { "models": [{ "id": "model", field: value }] } } })),
                "models[].{field}={value}"
            );
            assert!(
                !validator
                    .check(&json!({ "providers": { "local": { "modelOverrides": { "model": { field: value } } } } })),
                "modelOverrides.{field}={value}"
            );
        }
    }
    assert!(validator.check(&json!({
        "providers": {
            "local": {
                "models": [{ "id": "model", "contextWindow": 1, "maxTokens": 1 }],
                "modelOverrides": { "model": { "contextWindow": 1, "maxTokens": 1 } }
            }
        }
    })));
}

#[test]
fn validates_keybinding_syntax() {
    let validator = compile(keybindings_schema(), CompileOptions::default()).unwrap();
    for binding in [
        "a",
        "9",
        "pageUp",
        "+",
        "ctrl+shift+x",
        "alt+ctrl+?",
        "ctrl+shift+alt+super+f12",
    ] {
        assert!(
            validator.check(&json!({ "extension.action": binding })),
            "expected valid binding: {binding}"
        );
    }
    for binding in [
        "",
        "Ctrl+x",
        "control+x",
        "ctrl+not-a-key",
        "ctrl+ctrl+x",
        "ctrl+shift+alt+super+ctrl+x",
    ] {
        assert!(
            !validator.check(&json!({ "extension.action": binding })),
            "expected invalid binding: {binding}"
        );
    }
}

#[test]
fn rejects_settings_values_that_runtime_accessors_reject() {
    let validator = compile(settings_schema(), CompileOptions::default()).unwrap();
    let invalid_cases = [
        json!({ "compaction": { "reserveTokens": -1 } }),
        json!({ "compaction": { "keepRecentTokens": 1.5 } }),
        json!({ "compaction": { "modelOverrides": { "provider/model": { "reserveTokens": 9_007_199_254_740_992_i64 } } } }),
        json!({ "httpIdleTimeoutMs": -1 }),
        json!({ "websocketConnectTimeoutMs": -1 }),
        json!({ "httpIdleTimeoutMs": "bogus" }),
        json!({ "websocketConnectTimeoutMs": "30000" }),
        json!({ "codemode": { "inlineBudget": -1 } }),
    ];
    for invalid in invalid_cases {
        assert!(!validator.check(&invalid), "expected invalid settings: {invalid}");
    }

    assert!(validator.check(&json!({
        "compaction": {
            "reserveTokens": 0,
            "keepRecentTokens": 9_007_199_254_740_991_i64,
            "modelOverrides": { "provider/model": { "reserveTokens": 0 } }
        },
        "httpIdleTimeoutMs": 0,
        "websocketConnectTimeoutMs": "disabled",
        "codemode": { "inlineBudget": 0 }
    })));
}

#[tokio::test]
async fn accepts_custom_compatibility_settings_and_validates_schema_in_models_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("models.json");
    let path_str = path.to_str().unwrap();

    let compat = json!({
        "customOption": "value",
        "supportsStore": true,
        "supportsAdditionalTools": true
    });
    std::fs::write(
        &path,
        json!({ "providers": { "demo": { "api": "custom-api", "compat": compat } } }).to_string(),
    )
    .unwrap();

    let config = ModelConfig::load(Some(path_str)).await;
    assert_eq!(config.get_error(), None);
    let demo_compat = config.get_provider("demo").and_then(|p| p.compat.as_ref()).unwrap();
    assert_eq!(demo_compat["supportsStore"], json!(true));
    assert_eq!(demo_compat["supportsAdditionalTools"], json!(true));
    assert_eq!(demo_compat["customOption"], json!("value"));

    std::fs::write(
        &path,
        json!({ "providers": { "demo": { "compat": { "supportsLongCacheRetention": "yes" } } } }).to_string(),
    )
    .unwrap();
    let err_config = ModelConfig::load(Some(path_str)).await;
    assert!(
        err_config
            .get_error()
            .is_some_and(|e| e.contains("Invalid models.json schema"))
    );

    std::fs::write(
        &path,
        json!({
            "$schema": schema_url("models"),
            "providers": { "demo": { "baseUrl": "http://localhost:8080/v1", "models": [{ "id": "demo" }] } }
        })
        .to_string(),
    )
    .unwrap();
    let ok_schema = ModelConfig::load(Some(path_str)).await;
    assert_eq!(ok_schema.get_error(), None);
    assert_eq!(
        ok_schema
            .get_provider("demo")
            .and_then(|p| p.models.as_ref())
            .and_then(|m| m.first())
            .map(|m| m.id.as_str()),
        Some("demo")
    );

    std::fs::write(
        &path,
        json!({ "$schema": schema_url("models"), "providers": { "demo": { "models": [{ "id": "" }] } } }).to_string(),
    )
    .unwrap();
    assert!(
        ModelConfig::load(Some(path_str))
            .await
            .get_error()
            .is_some_and(|e| e.contains("Invalid models.json schema"))
    );

    std::fs::write(&path, json!({ "$schema": 42, "providers": {} }).to_string()).unwrap();
    assert!(
        ModelConfig::load(Some(path_str))
            .await
            .get_error()
            .is_some_and(|e| e.contains("$schema"))
    );
}

#[test]
#[serial]
fn accepts_schema_in_settings_and_keybindings_json() {
    let dir = TempDir::new().unwrap();
    let dir_str = dir.path().to_str().unwrap();

    std::fs::write(
        dir.path().join("settings.json"),
        json!({
            "$schema": schema_url("settings"),
            "theme": "light",
            "extensionSetting": { "enabled": true }
        })
        .to_string(),
    )
    .unwrap();

    let manager = SettingsManager::create(dir_str, Some(dir_str), None);
    assert_eq!(manager.get_theme().as_deref(), Some("light"));
    let global = manager.get_global_settings();
    assert_eq!(global.extra.get("$schema"), Some(&json!(schema_url("settings"))));
    assert_eq!(global.extra.get("extensionSetting"), Some(&json!({ "enabled": true })));

    std::fs::write(
        dir.path().join("keybindings.json"),
        json!({
            "$schema": schema_url("keybindings"),
            "app.session.new": "ctrl+n",
            "extension.invalid": "ctrl+not-a-key"
        })
        .to_string(),
    )
    .unwrap();

    let kb = KeybindingsManager::create(Some(dir_str));
    let user = kb.get_user_bindings();
    assert_eq!(user.len(), 1);
    assert!(user.contains_key("app.session.new"));
    assert!(!user.contains_key("$schema"));
    assert!(kb.get_effective_config().contains_key("app.session.new"));
}

#[test]
fn accepts_schema_in_themes_and_rejects_malformed_themes() {
    let dark_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/modes/interactive/theme/dark.json");
    let built_in: Value = serde_json::from_str(&std::fs::read_to_string(dark_path).unwrap()).unwrap();

    let mut with_schema = built_in.as_object().unwrap().clone();
    with_schema.insert("$schema".to_owned(), json!(schema_url("theme")));
    with_schema.insert("name".to_owned(), json!("custom"));
    let validated = validate_theme_json("custom", &Value::Object(with_schema)).unwrap();
    assert_eq!(validated.name, "custom");

    let mut slash_name = built_in.as_object().unwrap().clone();
    slash_name.insert("name".to_owned(), json!("invalid/name"));
    let err = validate_theme_json("invalid", &Value::Object(slash_name)).unwrap_err();
    assert!(err.to_string().contains("theme names cannot contain"));

    let mut empty_colors = built_in.as_object().unwrap().clone();
    empty_colors.insert("colors".to_owned(), json!({}));
    let err = validate_theme_json("invalid", &Value::Object(empty_colors)).unwrap_err();
    assert!(err.to_string().contains("Missing required color tokens"));

    let mut unexpected_root = built_in.as_object().unwrap().clone();
    unexpected_root.insert("unexpected".to_owned(), json!(true));

    let mut bad_colors_obj = built_in["colors"].as_object().unwrap().clone();
    bad_colors_obj.insert("scrollbarThmb".to_owned(), json!(""));
    let mut bad_color = built_in.as_object().unwrap().clone();
    bad_color.insert("colors".to_owned(), Value::Object(bad_colors_obj));

    let mut bad_export = built_in.as_object().unwrap().clone();
    bad_export.insert("export".to_owned(), json!({ "unexpected": "" }));

    for invalid in [unexpected_root, bad_color, bad_export] {
        let err = validate_theme_json("invalid", &Value::Object(invalid)).unwrap_err();
        assert!(
            err.to_string().contains("unexpected property"),
            "unexpected error text: {err}"
        );
    }
}

//! Port of packages/ai/test/openrouter-user-models.test.ts

use std::sync::Arc;

use pi_ai::auth::types::{ApiKeyCredential, Credential};
use pi_ai::models::{ModelsPublication, RefreshModelsContext};
use pi_ai::providers::openrouter::personalize_openrouter_catalog;
use pi_ai::types::{AnyModel, ClassifierModel, ImageModel, Model, ModelCost, ModelInput, ModelModality, ProviderEnv};
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{mock_fetch, response};
use serde_json::json;
use serial_test::serial;

fn chat_model(id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 200_000,
        max_tokens: 50_000,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn image_model() -> ImageModel {
    ImageModel {
        id: "image".to_owned(),
        name: "image".to_owned(),
        api: "openrouter-images".to_owned(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        output: vec![ModelModality::Image],
        cost: ModelCost::default(),
        headers: None,
    }
}

fn classifier_model() -> ClassifierModel {
    ClassifierModel {
        id: "classifier".to_owned(),
        name: "classifier".to_owned(),
        api: "typesafe-system-one".to_owned(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 64_000,
        headers: None,
        compat: None,
    }
}

fn refresh_context() -> RefreshModelsContext {
    RefreshModelsContext {
        credential: Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-or-test".to_owned()),
            env: Some(ProviderEnv::new()),
        })),
        stored: None,
        publish: Arc::new(|_: ModelsPublication| Box::pin(async { Ok(true) })),
        allow_network: true,
        force: None,
        signal: AbortController::new().signal(),
        base_url: None,
    }
}

#[tokio::test]
#[serial]
async fn keeps_keys_chat_models_with_limits_and_prices_and_passes_non_chat_through() {
    let payload = json!({
        "data": [
            {
                "id": "listed",
                "context_length": 1_000_000,
                "top_provider": { "context_length": 262_144, "max_completion_tokens": 32_768 },
                "pricing": { "prompt": "0.000003", "completion": "0.000015" },
            },
            { "id": "no-limits", "context_length": 0, "top_provider": { "context_length": null, "max_completion_tokens": null } },
            { "id": "router", "pricing": { "prompt": "-1", "completion": "-1" } },
        ]
    });
    let _guard = mock_fetch(move |_| {
        let body = pi_js::json::stringify(&payload);
        Box::pin(async move { Ok(response(200, &[("content-type", "application/json")], body)) })
    });

    let img = image_model();
    let cls = classifier_model();
    let catalog = vec![
        AnyModel::Chat(chat_model("listed")),
        AnyModel::Chat(chat_model("no-limits")),
        AnyModel::Chat(chat_model("router")),
        AnyModel::Chat(chat_model("blocked")),
        AnyModel::Image(img.clone()),
        AnyModel::Classifier(cls.clone()),
    ];

    let result = personalize_openrouter_catalog(&catalog, &refresh_context())
        .await
        .unwrap();
    let ids: Vec<&str> = result.iter().map(AnyModel::id).collect();
    assert_eq!(ids, vec!["listed", "no-limits", "router", "image", "classifier"]);

    let AnyModel::Chat(listed) = &result[0] else {
        panic!("expected chat")
    };
    assert_eq!(listed.context_window, 262_144);
    assert_eq!(listed.max_tokens, 32_768);
    assert_eq!(
        listed.cost,
        ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        }
    );

    let AnyModel::Chat(no_limits) = &result[1] else {
        panic!("expected chat")
    };
    assert_eq!(no_limits.context_window, 200_000);
    assert_eq!(no_limits.max_tokens, 50_000);
    assert_eq!(no_limits.cost, chat_model("x").cost);

    let AnyModel::Chat(router) = &result[2] else {
        panic!("expected chat")
    };
    assert_eq!(router.cost, ModelCost::default());
    assert_eq!(result[3], AnyModel::Image(img));
    assert_eq!(result[4], AnyModel::Classifier(cls));
}

#[tokio::test]
#[serial]
async fn returns_catalog_unchanged_when_endpoint_returns_401_403_or_404() {
    let catalog = vec![AnyModel::Chat(chat_model("a"))];
    for status in [401_u16, 403, 404] {
        let _guard = mock_fetch(move |_| Box::pin(async move { Ok(response(status, &[], "")) }));
        let res = personalize_openrouter_catalog(&catalog, &refresh_context())
            .await
            .unwrap();
        assert_eq!(res, catalog);
    }
}

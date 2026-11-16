//! Port of packages/evals/test/configured-runtime.test.ts

use indexmap::IndexMap;
use pi_ai::{
    Context, Message, ModelCostRates, ModelModality, ModelsSimpleStreamOptions, SimpleStreamOptions, StreamOptions,
    UserMessage, UserMessageContent,
};
use pi_coding_agent::core::model_runtime::ModelRuntime;
use pi_evals::evals::acme_server::{
    AcmeServer, AcmeServerMode, OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, OPENAI_PROBE_RESPONSE, OPENAI_PROVIDER_ID,
    create_acme_server,
};
use pi_evals::evals::configured_runtime::{
    AddedModelOutput, AddedModelResult, ModelFields, ProbeResponseFields, ProviderRuntimeOutput, ProviderRuntimeResult,
    ProviderScenario, inspect_added_model, inspect_provider, load_configured_model_runtime,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

const PROVIDER_ID: &str = OPENAI_PROVIDER_ID;

fn acme_model() -> ModelFields {
    ModelFields {
        id: OPENAI_MODEL_ID.to_string(),
        name: "Acme Chat".to_string(),
        provider: PROVIDER_ID.to_string(),
        reasoning: false,
        input: vec![ModelModality::Text],
        cost: ModelCostRates {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 32_768,
        max_tokens: 4_096,
    }
}

async fn agent_dir_with(models_json: &Value) -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let dir_path = dir.path().to_string_lossy().into_owned();
    let models_path = pi_js::path::join(&[&dir_path, "models.json"]);
    pi_js::fs::promises::write_file(
        &models_path,
        format!("{}\n", pi_js::json::stringify(models_json)).as_bytes(),
        None,
    )
    .await
    .unwrap();
    (dir, dir_path)
}

async fn runtime_with(models_json: &Value) -> (TempDir, ModelRuntime) {
    let (dir, dir_path) = agent_dir_with(models_json).await;
    let runtime = load_configured_model_runtime(&dir_path).await.unwrap();
    (dir, runtime)
}

fn acme_models_json(server: &AcmeServer) -> Value {
    let model = acme_model();
    json!({
        "providers": {
            PROVIDER_ID: {
                "baseUrl": server.base_url(),
                "api": "openai-completions",
                "apiKey": "$ACME_API_KEY",
                "models": [{
                    "id": model.id,
                    "name": model.name,
                    "reasoning": model.reasoning,
                    "input": model.input,
                    "cost": model.cost,
                    "contextWindow": model.context_window,
                    "maxTokens": model.max_tokens,
                }],
            },
        },
    })
}

fn probe(server: &AcmeServer, content: &str, api_key: &str) -> ProviderScenario {
    let content = content.to_string();
    let mut env = IndexMap::new();
    env.insert("ACME_API_KEY".to_string(), api_key.to_string());
    let server_clone = server.clone();
    ProviderScenario {
        provider_id: PROVIDER_ID.to_string(),
        model_id: OPENAI_MODEL_ID.to_string(),
        create_context: Box::new(move || Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text(content.clone()),
                timestamp: 0,
            })],
            tools: None,
        }),
        options: Some(ModelsSimpleStreamOptions {
            base: SimpleStreamOptions {
                base: StreamOptions {
                    env: Some(env),
                    max_tokens: Some(32),
                    ..Default::default()
                },
                ..Default::default()
            },
            transform_headers: None,
        }),
        valid_request_received: Box::new(move || server_clone.valid_request_received()),
    }
}

mod inspect_provider_tests {
    use super::*;

    #[tokio::test]
    async fn probes_a_models_json_provider_through_model_runtime() {
        let server = create_acme_server(AcmeServerMode::OpenAi);
        server.start().await.unwrap();
        let (_dir, runtime) = runtime_with(&acme_models_json(&server)).await;
        let out = inspect_provider(&runtime, probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ProviderRuntimeOutput {
                result: ProviderRuntimeResult::Success {
                    valid_request_received: true,
                    model: acme_model(),
                    response: ProbeResponseFields {
                        text: OPENAI_PROBE_RESPONSE.to_string(),
                        stop_reason: "stop".to_string(),
                        input_tokens: 3,
                        output_tokens: 2,
                    },
                },
            }
        );
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn keeps_a_non_probe_completion_distinct_from_a_valid_probe() {
        let server = create_acme_server(AcmeServerMode::OpenAi);
        server.start().await.unwrap();
        let (_dir, runtime) = runtime_with(&acme_models_json(&server)).await;
        let out = inspect_provider(&runtime, probe(&server, "hello", "resolved-acme-key"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ProviderRuntimeOutput {
                result: ProviderRuntimeResult::Success {
                    valid_request_received: false,
                    model: acme_model(),
                    response: ProbeResponseFields {
                        text: OPENAI_PROBE_RESPONSE.to_string(),
                        stop_reason: "stop".to_string(),
                        input_tokens: 3,
                        output_tokens: 2,
                    },
                },
            }
        );
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn returns_a_structured_error_when_the_configured_model_is_missing() {
        let server = create_acme_server(AcmeServerMode::OpenAi);
        server.start().await.unwrap();
        let (_dir, runtime) = runtime_with(&json!({ "providers": {} })).await;
        let out = inspect_provider(&runtime, probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ProviderRuntimeOutput {
                result: ProviderRuntimeResult::Error {
                    error: format!("Model {PROVIDER_ID}/{OPENAI_MODEL_ID} is unavailable after reload."),
                },
            }
        );
        assert!(!server.valid_request_received());
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn returns_a_structured_error_when_models_json_cannot_be_parsed() {
        let server = create_acme_server(AcmeServerMode::OpenAi);
        server.start().await.unwrap();
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        pi_js::fs::promises::write_file(&pi_js::path::join(&[&dir_path, "models.json"]), b"{", None)
            .await
            .unwrap();
        let runtime = load_configured_model_runtime(&dir_path).await.unwrap();
        let out = inspect_provider(&runtime, probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"))
            .await
            .unwrap();
        match out.result {
            ProviderRuntimeResult::Error { error } => {
                assert!(error.contains("Failed to parse models.json"), "{error}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!server.valid_request_received());
        server.stop().await.unwrap();
    }

    #[tokio::test]
    async fn does_not_treat_an_unauthorized_completion_as_a_valid_probe() {
        let server = create_acme_server(AcmeServerMode::OpenAi);
        server.start().await.unwrap();
        let (_dir, runtime) = runtime_with(&acme_models_json(&server)).await;
        let out = inspect_provider(&runtime, probe(&server, OPENAI_PROBE_PROMPT, "wrong-key"))
            .await
            .unwrap();
        assert_eq!(
            out,
            ProviderRuntimeOutput {
                result: ProviderRuntimeResult::Success {
                    valid_request_received: false,
                    model: acme_model(),
                    response: ProbeResponseFields {
                        text: String::new(),
                        stop_reason: "error".to_string(),
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                },
            }
        );
        server.stop().await.unwrap();
    }
}

mod inspect_added_model_tests {
    use super::*;

    const PROVIDER: &str = "openai";
    const MODEL_ID: &str = "fixture-chat";

    fn added_model() -> ModelFields {
        ModelFields {
            id: MODEL_ID.to_string(),
            name: "Fixture Chat".to_string(),
            provider: PROVIDER.to_string(),
            reasoning: true,
            input: vec![ModelModality::Text],
            cost: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 32_768,
            max_tokens: 4_096,
        }
    }

    #[tokio::test]
    async fn loads_an_added_model_without_dropping_built_in_models() {
        let model = added_model();
        let (_dir, runtime) = runtime_with(&json!({
            "providers": {
                PROVIDER: {
                    "models": [{
                        "id": model.id,
                        "name": model.name,
                        "reasoning": model.reasoning,
                        "input": model.input,
                        "cost": model.cost,
                        "contextWindow": model.context_window,
                        "maxTokens": model.max_tokens,
                    }],
                },
            },
        }))
        .await;
        let out = inspect_added_model(&runtime, PROVIDER, MODEL_ID).await.unwrap();
        assert_eq!(
            out,
            AddedModelOutput {
                result: AddedModelResult::Success {
                    model,
                    existing_models_preserved: true,
                },
            }
        );
    }

    #[tokio::test]
    async fn returns_a_structured_error_when_the_added_model_is_missing() {
        let (_dir, runtime) = runtime_with(&json!({ "providers": {} })).await;
        let out = inspect_added_model(&runtime, PROVIDER, MODEL_ID).await.unwrap();
        assert_eq!(
            out,
            AddedModelOutput {
                result: AddedModelResult::Error {
                    error: format!("Model {PROVIDER}/{MODEL_ID} is unavailable after reload."),
                },
            }
        );
    }

    #[tokio::test]
    async fn returns_a_structured_error_when_models_json_cannot_be_parsed() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        pi_js::fs::promises::write_file(&pi_js::path::join(&[&dir_path, "models.json"]), b"{", None)
            .await
            .unwrap();
        let runtime = load_configured_model_runtime(&dir_path).await.unwrap();
        let out = inspect_added_model(&runtime, PROVIDER, MODEL_ID).await.unwrap();
        match out.result {
            AddedModelResult::Error { error } => {
                assert!(error.contains("Failed to parse models.json"), "{error}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }
}

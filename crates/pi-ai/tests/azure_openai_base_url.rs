//! Port of packages/ai/test/azure-openai-base-url.test.ts

use std::sync::{Arc, Mutex};

use futures::stream;
use indexmap::IndexMap;
use pi_ai::api::azure_openai_responses::stream as stream_azure_openai_responses;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AnthropicMessagesCompat, ApiStreamOptions, ConstrainedSampling, ConstrainedSamplingConfig, Context, FetchFunction,
    JsonSchemaStrict, Message, ModelCompat, OpenAIResponsesCompat, ProviderHeaders, StopReason, StreamOptions, Tool,
    UserMessage, UserMessageContent,
};
use pi_js::env;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};
use serial_test::serial;

struct AzureEnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl AzureEnvGuard {
    fn clear_all() -> Self {
        const KEYS: &[&str] = &[
            "AZURE_OPENAI_BASE_URL",
            "AZURE_OPENAI_RESOURCE_NAME",
            "AZURE_OPENAI_API_VERSION",
            "AZURE_OPENAI_API_KEY",
        ];
        let mut saved = Vec::new();
        for &k in KEYS {
            saved.push((k, env::var(k)));
            env::remove_var(k);
        }
        Self { saved }
    }
}

impl Drop for AzureEnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

struct CapturedCall {
    url: String,
    headers: Headers,
    body: Value,
}

fn sse_from_values(events: &[Value]) -> String {
    events
        .iter()
        .map(|ev| format!("data: {}\n\n", pi_js::json::stringify(ev)))
        .collect()
}

fn capture_fetch(captured: Arc<Mutex<Vec<CapturedCall>>>, events: Vec<Value>) -> FetchFunction {
    let body_text = sse_from_values(&events);
    Arc::new(move |req: Request| {
        let cap = captured.clone();
        let sse = body_text.clone();
        Box::pin(async move {
            let body = req.body_json().unwrap_or(Value::Null);
            cap.lock().unwrap().push(CapturedCall {
                url: req.url,
                headers: req.headers,
                body,
            });
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                sse,
            ))
        })
    })
}

async fn capture_client_base_url(base_url: &str) -> String {
    let _guard = AzureEnvGuard::clear_all();
    env::set_var("AZURE_OPENAI_BASE_URL", base_url);
    let captured = Arc::new(Mutex::new(Vec::new()));
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-api-key".to_owned()),
                fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    calls[0].url.clone()
}

async fn capture_client_headers(headers: Option<ProviderHeaders>) -> Headers {
    let _guard = AzureEnvGuard::clear_all();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    headers,
                    fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("azureBaseUrl", json!("https://my-resource.openai.azure.com")),
        ),
    )
    .result()
    .await;

    let mut calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    calls.remove(0).headers
}

#[tokio::test]
#[serial]
async fn normalizes_cognitive_services_root_endpoints_to_openai_v1() {
    let url = capture_client_base_url("https://marc-quicktests-resource.cognitiveservices.azure.com").await;
    assert_eq!(
        url,
        "https://marc-quicktests-resource.cognitiveservices.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn normalizes_microsoft_foundry_root_endpoints_to_openai_v1() {
    let url = capture_client_base_url("https://marc-quicktests-resource.ai.azure.com").await;
    assert_eq!(
        url,
        "https://marc-quicktests-resource.ai.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn normalizes_azure_openai_root_endpoints_to_openai_v1() {
    let url = capture_client_base_url("https://my-resource.openai.azure.com").await;
    assert_eq!(
        url,
        "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn normalizes_openai_to_openai_v1() {
    let url = capture_client_base_url("https://my-resource.cognitiveservices.azure.com/openai").await;
    assert_eq!(
        url,
        "https://my-resource.cognitiveservices.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn preserves_openai_v1_endpoints() {
    let url = capture_client_base_url("https://my-resource.cognitiveservices.azure.com/openai/v1").await;
    assert_eq!(
        url,
        "https://my-resource.cognitiveservices.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn normalizes_openai_v1_responses_to_openai_v1() {
    let url = capture_client_base_url("https://my-resource.services.ai.azure.com/openai/v1/responses").await;
    assert_eq!(
        url,
        "https://my-resource.services.ai.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn preserves_explicit_non_azure_proxy_paths() {
    let url = capture_client_base_url("https://my-proxy.example.com/v1").await;
    assert_eq!(url, "https://my-proxy.example.com/v1/responses?api-version=v1");
}

#[tokio::test]
#[serial]
async fn strips_query_params_when_normalizing_azure_host_urls() {
    let url = capture_client_base_url("https://my-resource.openai.azure.com/openai?api-version=2024-12-01").await;
    assert_eq!(
        url,
        "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn preserves_query_params_on_non_azure_proxy_urls() {
    let url = capture_client_base_url("https://my-proxy.example.com/v1?custom=true").await;
    assert_eq!(
        url,
        "https://my-proxy.example.com/v1/responses?custom=true&api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn errors_on_invalid_urls() {
    let _guard = AzureEnvGuard::clear_all();
    env::set_var("AZURE_OPENAI_BASE_URL", "not-a-url");
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    let result = stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-api-key".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Invalid Azure OpenAI base URL")
    );
}

#[tokio::test]
#[serial]
async fn clamps_prompt_cache_key_to_openais_64_character_limit() {
    let _guard = AzureEnvGuard::clear_all();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    session_id: Some("x".repeat(67)),
                    fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("azureBaseUrl", json!("https://my-resource.openai.azure.com")),
        ),
    )
    .result()
    .await;

    let calls = captured.lock().unwrap();
    assert_eq!(
        calls[0].body.get("prompt_cache_key").and_then(Value::as_str),
        Some("x".repeat(64).as_str())
    );
}

#[tokio::test]
#[serial]
async fn disables_server_side_response_storage() {
    let _guard = AzureEnvGuard::clear_all();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("azureBaseUrl", json!("https://my-resource.openai.azure.com")),
        ),
    )
    .result()
    .await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls[0].body.get("store"), Some(&Value::Bool(false)));
}

#[tokio::test]
#[serial]
async fn honors_supports_strict_mode_false() {
    let _guard = AzureEnvGuard::clear_all();
    let mut model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    let mut compat = match model.compat {
        Some(ModelCompat::OpenAIResponses(c)) => c,
        _ => OpenAIResponsesCompat::default(),
    };
    compat.supports_strict_mode = Some(false);
    model.compat = Some(ModelCompat::OpenAIResponses(compat));

    let captured = Arc::new(Mutex::new(Vec::new()));
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: Some(vec![Tool {
            name: "preferred".to_owned(),
            description: "Preferred constrained tool".to_owned(),
            parameters: t::object([("value", t::string())]),
            constrained_sampling: Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrict::Prefer,
            })),
        }]),
    });

    stream_azure_openai_responses(
        &model,
        &ctx,
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("azureBaseUrl", json!("https://my-resource.openai.azure.com")),
        ),
    )
    .result()
    .await;

    let _unused: Option<AnthropicMessagesCompat> = None;
    let calls = captured.lock().unwrap();
    let tool = &calls[0].body.get("tools").and_then(Value::as_array).unwrap()[0];
    assert!(tool.get("strict").is_none());
}

#[tokio::test]
#[serial]
async fn builds_correct_default_url_from_azure_openai_resource_name() {
    let _guard = AzureEnvGuard::clear_all();
    env::set_var("AZURE_OPENAI_RESOURCE_NAME", "my-resource");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();
    stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-api-key".to_owned()),
                fetch: Some(capture_fetch(captured.clone(), Vec::new())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let calls = captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].url,
        "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1"
    );
}

#[tokio::test]
#[serial]
async fn forwards_parsed_events_in_order_before_normalizing_the_response() {
    let _guard = AzureEnvGuard::clear_all();
    let events = vec![
        json!({
            "type": "response.created",
            "sequence_number": 0,
            "response": { "id": "resp_azure" }
        }),
        json!({
            "type": "response.completed",
            "sequence_number": 1,
            "response": { "id": "resp_azure", "status": "completed" }
        }),
    ];
    let captured = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::new(Mutex::new(Vec::new()));
    let event_models = Arc::new(Mutex::new(Vec::new()));
    let rec_c = received.clone();
    let em_c = event_models.clone();
    let model = get_model("azure-openai-responses", "gpt-4o-mini").unwrap();

    let result = stream_azure_openai_responses(
        &model,
        &sample_context(),
        Some(
            ApiStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    fetch: Some(capture_fetch(captured, events.clone())),
                    on_provider_stream_event: Some(Arc::new(move |ev, ev_model| {
                        let r = rec_c.clone();
                        let e = em_c.clone();
                        let ev_c = ev.clone();
                        let m_c = ev_model.clone();
                        Box::pin(async move {
                            r.lock().unwrap().push(ev_c);
                            e.lock().unwrap().push(m_c);
                            Ok(())
                        })
                    })),
                    ..Default::default()
                },
                ..Default::default()
            }
            .with_extra("azureBaseUrl", json!("https://my-resource.openai.azure.com")),
        ),
    )
    .result()
    .await;

    assert_eq!(*received.lock().unwrap(), events);
    assert_eq!(*event_models.lock().unwrap(), vec![model.clone(), model]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.response_id.as_deref(), Some("resp_azure"));
}

#[tokio::test]
#[serial]
async fn uses_pis_user_agent_by_default_and_lets_explicit_headers_override() {
    let expected_ua = format!("pi ({} {}; {})", env::platform(), env::os_release(), env::arch());
    let default_headers = capture_client_headers(None).await;
    assert_eq!(default_headers.get("User-Agent").as_deref(), Some(expected_ua.as_str()));

    let mut overrides = IndexMap::new();
    overrides.insert("User-Agent".to_owned(), Some("custom-agent".to_owned()));
    let custom_headers = capture_client_headers(Some(overrides)).await;
    assert_eq!(custom_headers.get("User-Agent").as_deref(), Some("custom-agent"));
    let _unused = stream::empty::<()>();
}

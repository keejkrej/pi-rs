//! Port of packages/ai/test/anthropic-auth-token.test.ts (unit ai-api-anthropic)

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::anthropic_messages::{AnthropicClient, AnthropicOptions, stream, stream_with_options};
use pi_ai::auth::types::{ApiKeyAuthInput, AuthContext, AuthResult, ModelAuth};
use pi_ai::env_api_keys::{ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_OAUTH_TOKEN_ENV};
use pi_ai::models::{CreateModelsOptions, ModelsSimpleStreamOptions, create_models};
use pi_ai::providers::anthropic::anthropic_provider;
use pi_ai::types::{
    ApiStreamOptions, FetchFunction, Message, Model, ModelCost, ModelInput, ProviderHeaders, SimpleStreamOptions,
    StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::abort::AbortController;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::{Value, json};

struct EnvMapAuthContext(IndexMap<String, String>);

impl EnvMapAuthContext {
    fn from_pairs(pairs: &[(&str, &str)]) -> Arc<dyn AuthContext> {
        let mut map = IndexMap::new();
        for (k, v) in pairs {
            map.insert((*k).to_owned(), (*v).to_owned());
        }
        Arc::new(Self(map))
    }
}

#[async_trait::async_trait]
impl AuthContext for EnvMapAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn sse_body() -> String {
    [
        format!(
            "event: message_start\ndata: {}\n",
            pi_js::json::stringify(&json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            }))
        ),
        format!(
            "event: message_delta\ndata: {}\n",
            pi_js::json::stringify(&json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 1 }
            }))
        ),
        format!(
            "event: message_stop\ndata: {}\n",
            pi_js::json::stringify(&json!({ "type": "message_stop" }))
        ),
    ]
    .join("\n")
}

fn sse_http_response() -> Response {
    Response::new(
        200,
        Headers::from_pairs([("content-type", "text/event-stream")]),
        sse_body(),
    )
}

#[derive(Clone)]
struct CapturedWireRequest {
    headers: Headers,
    body: Value,
}

fn mock_capture_fetch(captured: Arc<Mutex<Option<CapturedWireRequest>>>) -> FetchFunction {
    Arc::new(move |req: Request| {
        let c = captured.clone();
        Box::pin(async move {
            let body = req.body_json().unwrap_or(Value::Null);
            *c.lock().unwrap() = Some(CapturedWireRequest {
                headers: req.headers,
                body,
            });
            Ok(sse_http_response())
        })
    })
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&pi_ai::types::Context {
        system_prompt: Some("System prompt.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

fn anthropic_model() -> Model {
    Model {
        r#type: None,
        id: "claude-test".to_owned(),
        name: "Claude Test".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        base_url: "https://api.anthropic.com".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 100_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn kimi_coding_model() -> Model {
    Model {
        id: "kimi-for-coding".to_owned(),
        name: "Kimi For Coding".to_owned(),
        provider: "kimi-coding".to_owned(),
        base_url: "https://api.kimi.com/coding".to_owned(),
        ..anthropic_model()
    }
}

#[tokio::test]
async fn resolves_anthropic_auth_token_as_a_bearer_authorization_header() {
    let provider = anthropic_provider().unwrap();
    let api_key_auth = provider.auth().api_key.as_ref().unwrap();
    let auth = api_key_auth
        .resolve(ApiKeyAuthInput {
            ctx: EnvMapAuthContext::from_pairs(&[
                ("ANTHROPIC_AUTH_TOKEN", "auth-token"),
                ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
                ("ANTHROPIC_API_KEY", "api-key"),
            ]),
            credential: None,
            signal: AbortController::new().signal(),
        })
        .await
        .unwrap()
        .unwrap();

    let mut expected_headers: ProviderHeaders = IndexMap::new();
    expected_headers.insert("Authorization".to_owned(), Some("Bearer auth-token".to_owned()));
    assert_eq!(
        auth,
        AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: Some(expected_headers),
                base_url: None,
            },
            env: None,
            source: Some(ANTHROPIC_AUTH_TOKEN_ENV.to_owned()),
        }
    );
}

#[tokio::test]
async fn preserves_anthropic_oauth_token_as_oauth_shaped_api_auth() {
    let provider = anthropic_provider().unwrap();
    let api_key_auth = provider.auth().api_key.as_ref().unwrap();
    let auth = api_key_auth
        .resolve(ApiKeyAuthInput {
            ctx: EnvMapAuthContext::from_pairs(&[
                ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
                ("ANTHROPIC_API_KEY", "api-key"),
            ]),
            credential: None,
            signal: AbortController::new().signal(),
        })
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        auth,
        AuthResult {
            auth: ModelAuth {
                api_key: Some("oauth-token".to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some(ANTHROPIC_OAUTH_TOKEN_ENV.to_owned()),
        }
    );
}

#[tokio::test]
async fn uses_authorization_headers_without_oauth_mode_request_shaping() {
    let captured = Arc::new(Mutex::new(None));
    let mut headers = IndexMap::new();
    headers.insert("Authorization".to_owned(), Some("Bearer gateway-token".to_owned()));

    stream(
        &anthropic_model(),
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                headers: Some(headers),
                fetch: Some(mock_capture_fetch(captured.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert!(req.headers.get("x-api-key").is_none());
    assert_eq!(
        req.headers.get("Authorization").as_deref(),
        Some("Bearer gateway-token")
    );
    assert!(
        !req.headers
            .get("anthropic-beta")
            .unwrap_or_default()
            .contains("oauth-2025-04-20")
    );
    let system = req.body.get("system").and_then(Value::as_array).unwrap();
    assert_eq!(system[0].get("text").and_then(Value::as_str), Some("System prompt."));
}

#[tokio::test]
async fn threads_auth_context_anthropic_auth_token_through_request_headers() {
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(EnvMapAuthContext::from_pairs(&[("ANTHROPIC_AUTH_TOKEN", "ctx-token")])),
        ..Default::default()
    }));
    models.set_provider(anthropic_provider().unwrap());

    models
        .stream_simple(
            &anthropic_model(),
            &pi_ai::types::Context {
                system_prompt: Some("System prompt.".to_owned()),
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hello".to_owned()),
                    timestamp: time::now_ms(),
                })],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        fetch: Some(mock_capture_fetch(captured.clone())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert!(req.headers.get("x-api-key").is_none());
    assert_eq!(req.headers.get("Authorization").as_deref(), Some("Bearer ctx-token"));
    assert!(
        !req.headers
            .get("anthropic-beta")
            .unwrap_or_default()
            .contains("oauth-2025-04-20")
    );
    let system = req.body.get("system").and_then(Value::as_array).unwrap();
    assert_eq!(system[0].get("text").and_then(Value::as_str), Some("System prompt."));
}

#[tokio::test]
async fn preserves_oauth_request_shaping_for_anthropic_oauth_token() {
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(EnvMapAuthContext::from_pairs(&[(
            "ANTHROPIC_OAUTH_TOKEN",
            "sk-ant-oat-test",
        )])),
        ..Default::default()
    }));
    models.set_provider(anthropic_provider().unwrap());

    models
        .stream_simple(
            &anthropic_model(),
            &pi_ai::types::Context {
                system_prompt: Some("System prompt.".to_owned()),
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hello".to_owned()),
                    timestamp: time::now_ms(),
                })],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        fetch: Some(mock_capture_fetch(captured.clone())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert!(req.headers.get("x-api-key").is_none());
    assert_eq!(
        req.headers.get("Authorization").as_deref(),
        Some("Bearer sk-ant-oat-test")
    );
    assert!(
        req.headers
            .get("anthropic-beta")
            .unwrap_or_default()
            .contains("oauth-2025-04-20")
    );
}

#[tokio::test]
async fn lets_explicit_request_headers_override_anthropic_auth_token() {
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(EnvMapAuthContext::from_pairs(&[("ANTHROPIC_AUTH_TOKEN", "ctx-token")])),
        ..Default::default()
    }));
    models.set_provider(anthropic_provider().unwrap());

    let mut headers = IndexMap::new();
    headers.insert("Authorization".to_owned(), Some("Bearer explicit-token".to_owned()));
    models
        .stream_simple(
            &anthropic_model(),
            &pi_ai::types::Context {
                system_prompt: Some("System prompt.".to_owned()),
                messages: vec![Message::User(UserMessage {
                    content: UserMessageContent::Text("Hello".to_owned()),
                    timestamp: time::now_ms(),
                })],
                tools: None,
            },
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        headers: Some(headers),
                        fetch: Some(mock_capture_fetch(captured.clone())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert_eq!(
        req.headers.get("Authorization").as_deref(),
        Some("Bearer explicit-token")
    );
}

#[tokio::test]
async fn uses_pis_user_agent_by_default_for_anthropic_messages_requests() {
    let captured = Arc::new(Mutex::new(None));
    let expected_ua = format!(
        "pi ({} {}; {})",
        pi_js::env::platform(),
        pi_js::env::os_release(),
        pi_js::env::arch()
    );

    stream(
        &anthropic_model(),
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("anthropic-key".to_owned()),
                fetch: Some(mock_capture_fetch(captured.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert_eq!(req.headers.get("User-Agent").as_deref(), Some(expected_ua.as_str()));
}

#[tokio::test]
async fn lets_explicit_headers_override_the_default_anthropic_messages_user_agent() {
    let captured = Arc::new(Mutex::new(None));
    let mut headers = IndexMap::new();
    headers.insert("User-Agent".to_owned(), Some("custom-client".to_owned()));

    stream(
        &kimi_coding_model(),
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("kimi-key".to_owned()),
                headers: Some(headers),
                fetch: Some(mock_capture_fetch(captured.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let req = captured.lock().unwrap().take().unwrap();
    assert_eq!(req.headers.get("User-Agent").as_deref(), Some("custom-client"));
}

#[tokio::test]
async fn preserves_explicit_anthropic_beta_header_replacement() {
    let create_params = Arc::new(Mutex::new(None));
    let cp_c = create_params.clone();
    let client = AnthropicClient::from_handler(move |params, _| {
        let cp = cp_c.clone();
        async move {
            *cp.lock().unwrap() = Some(params);
            Ok(sse_http_response())
        }
    });

    let mut headers = IndexMap::new();
    headers.insert("anthropic-beta".to_owned(), Some("custom-beta".to_owned()));
    stream_with_options(
        &anthropic_model(),
        &sample_context(),
        Some(AnthropicOptions {
            base: StreamOptions {
                api_key: Some("anthropic-key".to_owned()),
                headers: Some(headers),
                ..Default::default()
            },
            client: Some(client),
            ..Default::default()
        }),
    )
    .result()
    .await;

    let params = create_params.lock().unwrap().take().unwrap();
    assert_eq!(params.get("betas"), Some(&json!(["custom-beta"])));
}

#[tokio::test]
async fn preserves_explicit_anthropic_beta_header_suppression() {
    let create_params = Arc::new(Mutex::new(None));
    let cp_c = create_params.clone();
    let client = AnthropicClient::from_handler(move |params, _| {
        let cp = cp_c.clone();
        async move {
            *cp.lock().unwrap() = Some(params);
            Ok(sse_http_response())
        }
    });

    let mut headers = IndexMap::new();
    headers.insert("anthropic-beta".to_owned(), None);
    stream_with_options(
        &anthropic_model(),
        &sample_context(),
        Some(AnthropicOptions {
            base: StreamOptions {
                api_key: Some("anthropic-key".to_owned()),
                headers: Some(headers),
                ..Default::default()
            },
            client: Some(client),
            ..Default::default()
        }),
    )
    .result()
    .await;

    let params = create_params.lock().unwrap().take().unwrap();
    assert!(params.get("betas").is_none());
}

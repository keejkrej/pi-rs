//! Port of packages/ai/test/anthropic-federation.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::anthropic_messages::stream;
use pi_ai::auth::types::{ApiKeyAuthInput, AuthContext, AuthResult, ModelAuth};
use pi_ai::models::{CreateModelsOptions, ModelsSimpleStreamOptions, create_models};
use pi_ai::providers::anthropic::anthropic_provider;
use pi_ai::types::{
    ApiStreamOptions, FetchFunction, Message, Model, ModelCost, ModelInput, ProviderEnv, SimpleStreamOptions,
    StopReason, StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::abort::AbortController;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::{Value, json};
use serial_test::serial;

struct TempJwtFile {
    path: std::path::PathBuf,
}

impl TempJwtFile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pi-fed-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.jwt");
        std::fs::write(&path, "header.payload.signature").unwrap();
        Self { path }
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempJwtFile {
    fn drop(&mut self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}

struct EnvMapAuthContext(IndexMap<String, String>);

#[async_trait::async_trait]
impl AuthContext for EnvMapAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn base_federation_env(token_path: &str) -> ProviderEnv {
    let mut map = IndexMap::new();
    map.insert("ANTHROPIC_FEDERATION_RULE_ID".to_owned(), "fdrl_test".to_owned());
    map.insert("ANTHROPIC_ORGANIZATION_ID".to_owned(), "org-test".to_owned());
    map.insert("ANTHROPIC_SERVICE_ACCOUNT_ID".to_owned(), "svac_test".to_owned());
    map.insert("ANTHROPIC_IDENTITY_TOKEN_FILE".to_owned(), token_path.to_owned());
    map
}

fn sample_context() -> Context {
    Context {
        system_prompt: Some("System prompt.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    }
}

use pi_ai::types::Context;

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

fn sse_response() -> Response {
    let body = [
        format!(
            "event: message_start\ndata: {}\n",
            pi_js::json::stringify(&json!({
                "type": "message_start",
                "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } }
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
    .join("\n");
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

struct RecordedWireCall {
    url: String,
    headers: Headers,
    body: Value,
}

fn recording_fetch(calls: Arc<Mutex<Vec<RecordedWireCall>>>) -> FetchFunction {
    Arc::new(move |req: Request| {
        let c = calls.clone();
        Box::pin(async move {
            let body = req.body_json().unwrap_or(Value::Null);
            let is_token = req.url.ends_with("/v1/oauth/token");
            c.lock().unwrap().push(RecordedWireCall {
                url: req.url,
                headers: req.headers,
                body,
            });
            if is_token {
                Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({
                        "access_token": "fed-access-token",
                        "expires_in": 3600
                    })),
                ))
            } else {
                Ok(sse_response())
            }
        })
    })
}

async fn resolve_with_env(env: ProviderEnv) -> Option<AuthResult> {
    anthropic_provider()
        .unwrap()
        .auth()
        .api_key
        .as_ref()
        .unwrap()
        .resolve(ApiKeyAuthInput {
            ctx: Arc::new(EnvMapAuthContext(env)),
            credential: None,
            signal: AbortController::new().signal(),
        })
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn resolves_the_federation_variables_as_provider_env_with_no_request_auth() {
    let env = base_federation_env("/tmp/identity.jwt");
    let res = resolve_with_env(env.clone()).await.unwrap();
    assert_eq!(
        res,
        AuthResult {
            auth: ModelAuth::default(),
            env: Some(env),
            source: Some("workload identity federation".to_owned()),
        }
    );
}

#[tokio::test]
#[serial]
async fn passes_anthropic_workspace_id_through_when_set() {
    let mut env = base_federation_env("/tmp/identity.jwt");
    env.insert("ANTHROPIC_WORKSPACE_ID".to_owned(), "wrkspc_test".to_owned());
    let res = resolve_with_env(env).await.unwrap();
    assert_eq!(
        res.env
            .as_ref()
            .and_then(|e| e.get("ANTHROPIC_WORKSPACE_ID"))
            .map(String::as_str),
        Some("wrkspc_test")
    );
}

#[tokio::test]
#[serial]
async fn is_not_configured_when_a_federation_variable_is_missing() {
    let mut env = base_federation_env("/tmp/identity.jwt");
    env.shift_remove("ANTHROPIC_IDENTITY_TOKEN_FILE");
    assert!(resolve_with_env(env).await.is_none());
}

#[tokio::test]
#[serial]
async fn treats_anthropic_service_account_id_as_optional_like_the_sdk() {
    let jwt = TempJwtFile::new("optional-sa");
    let mut partial = base_federation_env(&jwt.path_str());
    partial.shift_remove("ANTHROPIC_SERVICE_ACCOUNT_ID");

    assert_eq!(
        resolve_with_env(partial.clone()).await.unwrap(),
        AuthResult {
            auth: ModelAuth::default(),
            env: Some(partial.clone()),
            source: Some("workload identity federation".to_owned()),
        }
    );

    let calls = Arc::new(Mutex::new(Vec::new()));
    stream(
        &anthropic_model(),
        &normalize_context(&sample_context()),
        Some(ApiStreamOptions {
            base: StreamOptions {
                env: Some(partial),
                fetch: Some(recording_fetch(calls.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let guard = calls.lock().unwrap();
    let token_call = guard.iter().find(|c| c.url.ends_with("/v1/oauth/token")).unwrap();
    assert_eq!(token_call.body.get("organization_id"), Some(&json!("org-test")));
    assert_eq!(token_call.body.get("federation_rule_id"), Some(&json!("fdrl_test")));
    assert!(token_call.body.get("service_account_id").is_none());
}

#[tokio::test]
#[serial]
async fn keeps_api_key_and_auth_token_precedence_over_federation() {
    let mut env_api = base_federation_env("/tmp/identity.jwt");
    env_api.insert("ANTHROPIC_API_KEY".to_owned(), "api-key".to_owned());
    assert_eq!(
        resolve_with_env(env_api).await.unwrap(),
        AuthResult {
            auth: ModelAuth {
                api_key: Some("api-key".to_owned()),
                headers: None,
                base_url: None,
            },
            env: None,
            source: Some("ANTHROPIC_API_KEY".to_owned()),
        }
    );

    let mut env_tok = base_federation_env("/tmp/identity.jwt");
    env_tok.insert("ANTHROPIC_AUTH_TOKEN".to_owned(), "auth-token".to_owned());
    let mut expected_headers = IndexMap::new();
    expected_headers.insert("Authorization".to_owned(), Some("Bearer auth-token".to_owned()));
    assert_eq!(
        resolve_with_env(env_tok).await.unwrap(),
        AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: Some(expected_headers),
                base_url: None,
            },
            env: None,
            source: Some("ANTHROPIC_AUTH_TOKEN".to_owned()),
        }
    );
}

#[tokio::test]
#[serial]
async fn exchanges_federation_identity_and_sends_bearer_token_instead_of_api_key() {
    let jwt = TempJwtFile::new("hand-off");
    let env = base_federation_env(&jwt.path_str());
    let calls = Arc::new(Mutex::new(Vec::new()));

    stream(
        &anthropic_model(),
        &normalize_context(&sample_context()),
        Some(ApiStreamOptions {
            base: StreamOptions {
                env: Some(env),
                fetch: Some(recording_fetch(calls.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let guard = calls.lock().unwrap();
    let token_call = guard.iter().find(|c| c.url.ends_with("/v1/oauth/token")).unwrap();
    assert_eq!(token_call.body.get("federation_rule_id"), Some(&json!("fdrl_test")));
    assert_eq!(token_call.body.get("organization_id"), Some(&json!("org-test")));
    assert_eq!(token_call.body.get("service_account_id"), Some(&json!("svac_test")));
    assert_eq!(
        token_call.body.get("assertion"),
        Some(&json!("header.payload.signature"))
    );

    let msg_call = guard.iter().find(|c| c.url.contains("/v1/messages")).unwrap();
    assert!(msg_call.headers.get("x-api-key").is_none());
    assert_eq!(
        msg_call.headers.get("authorization").as_deref(),
        Some("Bearer fed-access-token")
    );
}

#[tokio::test]
#[serial]
async fn threads_auth_context_federation_variables_through_models() {
    let jwt = TempJwtFile::new("models-thread");
    let env = base_federation_env(&jwt.path_str());
    let models = create_models(Some(CreateModelsOptions {
        auth_context: Some(Arc::new(EnvMapAuthContext(env))),
        ..Default::default()
    }));
    models.set_provider(anthropic_provider().unwrap());

    let calls = Arc::new(Mutex::new(Vec::new()));
    let msg = models
        .stream_simple(
            &anthropic_model(),
            &sample_context(),
            Some(ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        fetch: Some(recording_fetch(calls.clone())),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                transform_headers: None,
            }),
        )
        .result()
        .await;

    assert_eq!(msg.stop_reason, StopReason::Stop);
    let guard = calls.lock().unwrap();
    let token_call = guard.iter().find(|c| c.url.ends_with("/v1/oauth/token")).unwrap();
    assert_eq!(token_call.body.get("federation_rule_id"), Some(&json!("fdrl_test")));
    let msg_call = guard.iter().find(|c| c.url.contains("/v1/messages")).unwrap();
    assert_eq!(
        msg_call.headers.get("authorization").as_deref(),
        Some("Bearer fed-access-token")
    );
}

#[tokio::test]
#[serial]
async fn lets_an_explicit_api_key_win_over_federation_env() {
    let jwt = TempJwtFile::new("explicit-key");
    let env = base_federation_env(&jwt.path_str());
    let calls = Arc::new(Mutex::new(Vec::new()));

    stream(
        &anthropic_model(),
        &normalize_context(&sample_context()),
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("explicit-key".to_owned()),
                env: Some(env),
                fetch: Some(recording_fetch(calls.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    let guard = calls.lock().unwrap();
    assert!(guard.iter().all(|c| !c.url.ends_with("/v1/oauth/token")));
    assert_eq!(guard[0].headers.get("x-api-key").as_deref(), Some("explicit-key"));
}

#[tokio::test]
#[serial]
async fn does_not_federate_other_anthropic_messages_providers() {
    let jwt = TempJwtFile::new("kimi-no-fed");
    let env = base_federation_env(&jwt.path_str());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let kimi = Model {
        provider: "kimi-coding".to_owned(),
        base_url: "https://api.kimi.com/coding".to_owned(),
        ..anthropic_model()
    };

    let msg = stream(
        &kimi,
        &normalize_context(&sample_context()),
        Some(ApiStreamOptions {
            base: StreamOptions {
                env: Some(env),
                fetch: Some(recording_fetch(calls.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(msg.stop_reason, StopReason::Error);
    assert!(calls.lock().unwrap().is_empty());
}

//! Port of packages/ai/test/anthropic-federation-sdk.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::anthropic_messages::stream;
use pi_ai::types::{
    ApiStreamOptions, Context, FetchFunction, Message, Model, ModelCost, ModelInput, ProviderEnv, StopReason,
    StreamOptions, UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::json;
use serial_test::serial;

struct EnvMapGuard {
    previous: Vec<(String, Option<String>)>,
}

impl EnvMapGuard {
    fn set(pairs: &ProviderEnv) -> Self {
        let mut previous = Vec::new();
        for (k, v) in pairs {
            previous.push((k.clone(), pi_js::env::var(k)));
            pi_js::env::set_var(k, v);
        }
        Self { previous }
    }
}

impl Drop for EnvMapGuard {
    fn drop(&mut self) {
        for (k, prev) in &self.previous {
            match prev {
                Some(v) => pi_js::env::set_var(k, v),
                None => pi_js::env::remove_var(k),
            }
        }
    }
}

struct TempJwtFile {
    path: std::path::PathBuf,
}

impl TempJwtFile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("pi-fed-sdk-{name}-{}", std::process::id()));
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedRequest {
    path: String,
    authorization: Option<String>,
}

fn create_fetch(requests: Arc<Mutex<Vec<RecordedRequest>>>) -> FetchFunction {
    Arc::new(move |req: Request| {
        let r = requests.clone();
        Box::pin(async move {
            let parsed = url::Url::parse(&req.url).unwrap();
            let path = parsed.path().to_owned();
            let authorization = req.headers.get("authorization");
            r.lock().unwrap().push(RecordedRequest {
                path: path.clone(),
                authorization,
            });
            if path == "/v1/oauth/token" {
                return Ok(Response::new(
                    200,
                    Headers::from_pairs([("content-type", "application/json")]),
                    pi_js::json::stringify(&json!({
                        "access_token": "federated-token",
                        "expires_in": 3600
                    })),
                ));
            }
            Ok(sse_response())
        })
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
        sampling_params_by_thinking_level: None,
    }
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("System prompt.".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    })
}

#[tokio::test]
#[serial]
async fn exchanges_the_identity_token_once_across_requests() {
    let jwt = TempJwtFile::new("cache-once");
    let mut env = IndexMap::new();
    env.insert("ANTHROPIC_FEDERATION_RULE_ID".to_owned(), "fdrl_test".to_owned());
    env.insert("ANTHROPIC_ORGANIZATION_ID".to_owned(), "org-test".to_owned());
    env.insert("ANTHROPIC_IDENTITY_TOKEN_FILE".to_owned(), jwt.path_str());

    let requests = Arc::new(Mutex::new(Vec::new()));
    let fetch_fn = create_fetch(requests.clone());
    let model = anthropic_model();
    let ctx = sample_context();

    for _ in 0..3 {
        let msg = stream(
            &model,
            &ctx,
            Some(ApiStreamOptions {
                base: StreamOptions {
                    env: Some(env.clone()),
                    fetch: Some(fetch_fn.clone()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .result()
        .await;
        assert_eq!(msg.stop_reason, StopReason::Stop);
    }

    let guard = requests.lock().unwrap();
    let token_reqs: Vec<_> = guard.iter().filter(|r| r.path == "/v1/oauth/token").collect();
    assert_eq!(token_reqs.len(), 1);
    let msg_reqs: Vec<_> = guard.iter().filter(|r| r.path == "/v1/messages").collect();
    assert_eq!(msg_reqs.len(), 3);
    for req in msg_reqs {
        assert_eq!(req.authorization.as_deref(), Some("Bearer federated-token"));
    }
}

#[tokio::test]
#[serial]
async fn does_not_run_the_sdk_credential_chain_for_header_owned_auth() {
    let jwt = TempJwtFile::new("header-owned");
    let mut env = IndexMap::new();
    env.insert("ANTHROPIC_FEDERATION_RULE_ID".to_owned(), "fdrl_test".to_owned());
    env.insert("ANTHROPIC_ORGANIZATION_ID".to_owned(), "org-test".to_owned());
    env.insert("ANTHROPIC_IDENTITY_TOKEN_FILE".to_owned(), jwt.path_str());
    let _env_guard = EnvMapGuard::set(&env);

    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut headers = IndexMap::new();
    headers.insert("Authorization".to_owned(), Some("Bearer auth-token".to_owned()));

    let msg = stream(
        &anthropic_model(),
        &sample_context(),
        Some(ApiStreamOptions {
            base: StreamOptions {
                headers: Some(headers),
                fetch: Some(create_fetch(requests.clone())),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(
        *requests.lock().unwrap(),
        vec![RecordedRequest {
            path: "/v1/messages".to_owned(),
            authorization: Some("Bearer auth-token".to_owned()),
        }]
    );
}

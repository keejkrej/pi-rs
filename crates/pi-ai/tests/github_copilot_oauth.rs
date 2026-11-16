//! Port of packages/ai/test/github-copilot-oauth.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::oauth::github_copilot::github_copilot_oauth;
use pi_ai::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthType, Credential, CredentialStore, OAuthCredential,
    ProviderAuthInteraction,
};
use pi_ai::models::{CreateModelsOptions, create_models};
use pi_ai::providers::github_copilot::github_copilot_provider;
use pi_ai::types::Model;
use pi_js::abort::AbortSignal;
use pi_js::fetch::testing::mock_fetch;
use pi_js::fetch::{Headers, Response};
use pi_js::seam::Guard;
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};
use pi_js::{BoxFuture, Error, Result};

const TEST_COPILOT_ACCESS_TOKEN: &str = "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;";
const TEST_COPILOT_MODELS_URL: &str = "https://api.individual.githubcopilot.com/models";

fn json_response(body: &Value, status: u16, extra_headers: &[(&str, &str)]) -> Response {
    let mut headers = Vec::with_capacity(extra_headers.len() + 1);
    headers.push(("Content-Type", "application/json"));
    headers.extend_from_slice(extra_headers);
    Response::new(status, Headers::from_pairs(headers), pi_js::json::stringify(body))
}

fn require_model_id(models: &[Model], index: usize) -> String {
    models
        .get(index)
        .unwrap_or_else(|| panic!("Expected a GitHub Copilot model at index {index}"))
        .id
        .clone()
}

#[derive(Clone, Debug, PartialEq)]
struct RecordedDeviceCode {
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
}

type PromptFn = Arc<dyn Fn(String, Option<String>) -> BoxFuture<Result<String>> + Send + Sync>;
type DeviceCodeFn = Arc<dyn Fn(RecordedDeviceCode) + Send + Sync>;
type ProgressFn = Arc<dyn Fn(String) + Send + Sync>;

struct TestCopilotInteraction {
    signal: AbortSignal,
    on_prompt: PromptFn,
    on_device_code: DeviceCodeFn,
    on_progress: Option<ProgressFn>,
}

#[async_trait::async_trait]
impl AuthInteraction for TestCopilotInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        match prompt.kind {
            AuthPromptKind::Text { message, placeholder } => (self.on_prompt)(message, placeholder).await,
            other => panic!("Unexpected prompt: {other:?}"),
        }
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            } => (self.on_device_code)(RecordedDeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            }),
            AuthEvent::Progress { message } => {
                if let Some(cb) = &self.on_progress {
                    cb(message);
                }
            }
            _ => {}
        }
    }
}

fn login_github_copilot_for_test(
    on_device_code: DeviceCodeFn,
    on_prompt: PromptFn,
    on_progress: Option<ProgressFn>,
) -> impl std::future::Future<Output = Result<OAuthCredential>> {
    let signal = AbortSignal::never();
    let interaction = Arc::new(TestCopilotInteraction {
        signal: signal.clone(),
        on_prompt,
        on_device_code,
        on_progress,
    });
    async move {
        github_copilot_oauth()
            .login(ProviderAuthInteraction::new(interaction, signal), None)
            .await
    }
}

fn stub_github_copilot_login_fetch(
    models_resp: Arc<dyn Fn() -> Response + Send + Sync>,
    policy_resp: Option<Arc<dyn Fn(&str) -> Result<Response> + Send + Sync>>,
) -> Guard {
    mock_fetch(move |req| {
        let models_resp = Arc::clone(&models_resp);
        let policy_resp = policy_resp.clone();
        Box::pin(async move {
            let url = req.url.as_str();
            if url.ends_with("/login/device/code") {
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 1,
                        "expires_in": 900,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/login/oauth/access_token") {
                return Ok(json_response(&json!({ "access_token": "ghu_refresh_token" }), 200, &[]));
            }
            if url.contains("/copilot_internal/v2/token") {
                return Ok(json_response(
                    &json!({ "token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999_i64 }),
                    200,
                    &[],
                ));
            }
            if url == TEST_COPILOT_MODELS_URL {
                return Ok(models_resp());
            }
            if let Some(rest) = url.strip_prefix(&format!("{TEST_COPILOT_MODELS_URL}/")) {
                if let Some(model_id) = rest.strip_suffix("/policy") {
                    if let Some(cb) = policy_resp {
                        return cb(model_id);
                    }
                    panic!("Unexpected policy request: {url}");
                }
            }
            panic!("Unexpected fetch URL: {url}");
        })
    })
}

async fn refresh_github_copilot_models_for_test(data: Value, proxy_host: &str) -> Result<OAuthCredential> {
    let access_token = format!("tid=test;exp=9999999999;proxy-ep={proxy_host};");
    let models_url = format!("https://{}/models", proxy_host.replacen("proxy.", "api.", 1));
    let _guard = mock_fetch(move |req| {
        let access_token = access_token.clone();
        let models_url = models_url.clone();
        let data = data.clone();
        Box::pin(async move {
            if req.url.contains("/copilot_internal/v2/token") {
                return Ok(json_response(
                    &json!({ "token": access_token, "expires_at": 9999999999_i64 }),
                    200,
                    &[],
                ));
            }
            if req.url == models_url {
                assert_eq!(
                    req.headers.get("authorization").as_deref(),
                    Some(format!("Bearer {access_token}").as_str())
                );
                return Ok(json_response(&json!({ "data": data }), 200, &[]));
            }
            panic!("Unexpected fetch URL: {}", req.url);
        })
    });

    github_copilot_oauth()
        .refresh(
            OAuthCredential::new("ghu_refresh_token", "old-access-token", 0),
            AbortSignal::never(),
        )
        .await
}

#[tokio::test]
async fn filters_models_to_the_authenticated_account_picker_catalog() {
    let provider = github_copilot_provider();
    let provider_models = provider.get_models();
    let picker_model_id = require_model_id(&provider_models, 0);
    let disabled_model_id = require_model_id(&provider_models, 1);
    let hidden_model_id = require_model_id(&provider_models, 2);

    let credentials = refresh_github_copilot_models_for_test(
        json!([
            {
                "id": picker_model_id,
                "model_picker_enabled": true,
                "capabilities": { "supports": { "tool_calls": true } },
            },
            {
                "id": disabled_model_id,
                "model_picker_enabled": true,
                "policy": { "state": "disabled" },
                "capabilities": { "supports": { "tool_calls": true } },
            },
            {
                "id": hidden_model_id,
                "model_picker_enabled": false,
                "policy": { "state": "enabled" },
                "capabilities": { "supports": { "tool_calls": true } },
            },
        ]),
        "proxy.individual.githubcopilot.com",
    )
    .await
    .unwrap();

    assert_eq!(
        credentials.extra.get("availableModelIds"),
        Some(&json!([picker_model_id]))
    );

    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify_with(
            "github-copilot",
            move |_| async move { Ok(Some(Credential::OAuth(credentials))) },
            None,
        )
        .await
        .unwrap();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(store as Arc<dyn CredentialStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(provider);

    let available_ids: Vec<String> = models
        .get_available(Some("github-copilot"), None)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(available_ids, vec![picker_model_id]);
}

#[tokio::test]
async fn falls_back_to_explicitly_enabled_policy_models_when_the_picker_catalog_is_empty() {
    let provider = github_copilot_provider();
    let enabled_model_id = require_model_id(&provider.get_models(), 0);
    let credentials = refresh_github_copilot_models_for_test(
        json!([
            {
                "id": enabled_model_id,
                "model_picker_enabled": false,
                "policy": { "state": "enabled" },
                "capabilities": { "supports": { "tool_calls": true } },
            },
            {
                "id": "policy-disabled-model",
                "model_picker_enabled": false,
                "policy": { "state": "disabled" },
                "capabilities": { "supports": { "tool_calls": true } },
            },
            {
                "id": "unconfigured-model",
                "model_picker_enabled": false,
                "capabilities": { "supports": { "tool_calls": true } },
            },
            {
                "id": "tool-incapable-model",
                "model_picker_enabled": false,
                "policy": { "state": "enabled" },
                "capabilities": { "supports": { "tool_calls": false } },
            },
        ]),
        "proxy.individual.githubcopilot.com",
    )
    .await
    .unwrap();

    assert_eq!(
        credentials.extra.get("availableModelIds"),
        Some(&json!([enabled_model_id]))
    );

    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify_with(
            "github-copilot",
            move |_| async move { Ok(Some(Credential::OAuth(credentials))) },
            None,
        )
        .await
        .unwrap();
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(store as Arc<dyn CredentialStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(provider);

    let available_ids: Vec<String> = models
        .get_available(Some("github-copilot"), None)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(available_ids, vec![enabled_model_id]);
}

#[tokio::test]
async fn does_not_fall_back_to_policy_models_for_non_individual_accounts() {
    let credentials = refresh_github_copilot_models_for_test(
        json!([
            {
                "id": "gpt-4.1",
                "model_picker_enabled": false,
                "policy": { "state": "enabled" },
                "capabilities": { "supports": { "tool_calls": true } },
            }
        ]),
        "proxy.business.githubcopilot.com",
    )
    .await
    .unwrap();

    assert_eq!(credentials.extra.get("availableModelIds"), Some(&json!([])));
}

#[tokio::test]
async fn does_not_retry_model_catalog_throttling_during_credential_refresh() {
    let catalog_request_count = Arc::new(Mutex::new(0_usize));
    let count_ref = Arc::clone(&catalog_request_count);
    let _guard = mock_fetch(move |req| {
        let count_ref = Arc::clone(&count_ref);
        Box::pin(async move {
            if req.url.contains("/copilot_internal/v2/token") {
                return Ok(json_response(
                    &json!({ "token": TEST_COPILOT_ACCESS_TOKEN, "expires_at": 9999999999_i64 }),
                    200,
                    &[],
                ));
            }
            if req.url == TEST_COPILOT_MODELS_URL {
                *count_ref.lock().unwrap() += 1;
                return Ok(json_response(
                    &json!({ "error": "too many requests" }),
                    429,
                    &[("Retry-After", "0")],
                ));
            }
            panic!("Unexpected fetch URL: {}", req.url);
        })
    });

    let err = github_copilot_oauth()
        .refresh(
            OAuthCredential::new("ghu_refresh_token", "old-access-token", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("429"), "unexpected error: {err}");
    assert_eq!(*catalog_request_count.lock().unwrap(), 1);
}

#[tokio::test(start_paused = true)]
async fn reports_device_code_details_through_on_device_code() {
    set_system_time(parse_date_utc("2026-03-09T00:00:00Z").unwrap());
    let _guard = stub_github_copilot_login_fetch(
        Arc::new(|| json_response(&json!({ "data": [] }), 200, &[])),
        Some(Arc::new(|_| Ok(Response::new(200, Headers::new(), "")))),
    );

    let device_codes = Arc::new(Mutex::new(Vec::<RecordedDeviceCode>::new()));
    let dc_ref = Arc::clone(&device_codes);
    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(move |info| dc_ref.lock().unwrap().push(info)),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));

    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert_eq!(
        *device_codes.lock().unwrap(),
        vec![RecordedDeviceCode {
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: "https://github.com/login/device".to_owned(),
            interval_seconds: Some(1.0),
            expires_in_seconds: Some(900.0),
        }]
    );

    tokio::time::advance(Duration::from_millis(1000)).await;
    let _ = handle.await.unwrap().unwrap();
    clear_system_time();
}

#[tokio::test(start_paused = true)]
async fn updates_only_known_tool_capable_unconfigured_account_model_policies() {
    let provider_models = github_copilot_provider().get_models();
    let configured_model_id = require_model_id(&provider_models, 0);
    let unconfigured_model_id = require_model_id(&provider_models, 1);
    let tool_incapable_model_id = require_model_id(&provider_models, 2);

    let catalog_request_count = Arc::new(Mutex::new(0_usize));
    let cat_count_ref = Arc::clone(&catalog_request_count);
    let policy_model_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let policy_ids_ref = Arc::clone(&policy_model_ids);

    let c_id = configured_model_id.clone();
    let u_id = unconfigured_model_id.clone();
    let t_id = tool_incapable_model_id.clone();

    let _guard = stub_github_copilot_login_fetch(
        Arc::new(move || {
            *cat_count_ref.lock().unwrap() += 1;
            json_response(
                &json!({
                    "data": [
                        {
                            "id": c_id,
                            "model_picker_enabled": true,
                            "policy": { "state": "enabled" },
                            "capabilities": { "supports": { "tool_calls": true } },
                        },
                        {
                            "id": u_id,
                            "model_picker_enabled": true,
                            "policy": { "state": "unconfigured" },
                            "capabilities": { "supports": { "tool_calls": true } },
                        },
                        {
                            "id": "remote-only-model",
                            "model_picker_enabled": true,
                            "policy": { "state": "unconfigured" },
                            "capabilities": { "supports": { "tool_calls": true } },
                        },
                        {
                            "id": t_id,
                            "model_picker_enabled": true,
                            "policy": { "state": "unconfigured" },
                            "capabilities": { "supports": { "tool_calls": false } },
                        },
                    ]
                }),
                200,
                &[],
            )
        }),
        Some(Arc::new(move |model_id| {
            policy_ids_ref.lock().unwrap().push(model_id.to_owned());
            Ok(Response::new(200, Headers::new(), ""))
        })),
    );

    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(|_| {}),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    let _ = handle.await.unwrap().unwrap();

    assert_eq!(*catalog_request_count.lock().unwrap(), 1);
    assert_eq!(*policy_model_ids.lock().unwrap(), vec![unconfigured_model_id]);
}

#[tokio::test(start_paused = true)]
async fn retries_a_throttled_policy_update_after_retry_after() {
    let model_id = require_model_id(&github_copilot_provider().get_models(), 0);
    let policy_request_count = Arc::new(Mutex::new(0_usize));
    let count_ref = Arc::clone(&policy_request_count);

    let _guard = stub_github_copilot_login_fetch(
        Arc::new(move || {
            json_response(
                &json!({ "data": [{ "id": model_id, "model_picker_enabled": true, "policy": { "state": "unconfigured" } }] }),
                200,
                &[],
            )
        }),
        Some(Arc::new(move |_| {
            let mut guard = count_ref.lock().unwrap();
            *guard += 1;
            if *guard == 1 {
                Ok(json_response(
                    &json!({ "error": "too many requests" }),
                    429,
                    &[("Retry-After", "1")],
                ))
            } else {
                Ok(Response::new(200, Headers::new(), ""))
            }
        })),
    );

    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(|_| {}),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    tokio::task::yield_now().await;
    assert_eq!(*policy_request_count.lock().unwrap(), 1);

    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*policy_request_count.lock().unwrap(), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = handle.await.unwrap().unwrap();
    assert_eq!(*policy_request_count.lock().unwrap(), 2);
}

#[tokio::test(start_paused = true)]
async fn continues_policy_updates_after_a_transport_failure() {
    let provider_models = github_copilot_provider().get_models();
    let model_ids = vec![
        require_model_id(&provider_models, 0),
        require_model_id(&provider_models, 1),
    ];
    let policy_model_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let ids_ref = Arc::clone(&policy_model_ids);
    let catalog_ids = model_ids.clone();

    let _guard = stub_github_copilot_login_fetch(
        Arc::new(move || {
            let data: Vec<Value> = catalog_ids
                .iter()
                .map(|id| json!({ "id": id, "model_picker_enabled": true, "policy": { "state": "unconfigured" } }))
                .collect();
            json_response(&json!({ "data": data }), 200, &[])
        }),
        Some(Arc::new(move |model_id| {
            let mut guard = ids_ref.lock().unwrap();
            guard.push(model_id.to_owned());
            if guard.len() == 1 {
                return Err(Error::msg("fetch failed"));
            }
            Ok(Response::new(200, Headers::new(), ""))
        })),
    );

    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(|_| {}),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    let _ = handle.await.unwrap().unwrap();

    assert_eq!(*policy_model_ids.lock().unwrap(), model_ids);
}

#[tokio::test(start_paused = true)]
async fn stops_policy_updates_and_persists_authentication_when_retry_delay_exceeds_budget() {
    let provider_models = github_copilot_provider().get_models();
    let first_model_id = require_model_id(&provider_models, 0);
    let second_model_id = require_model_id(&provider_models, 1);
    let policy_model_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let ids_ref = Arc::clone(&policy_model_ids);

    let f_id = first_model_id.clone();
    let s_id = second_model_id.clone();
    let _guard = stub_github_copilot_login_fetch(
        Arc::new(move || {
            json_response(
                &json!({
                    "data": [
                        { "id": f_id, "model_picker_enabled": true, "policy": { "state": "unconfigured" } },
                        { "id": s_id, "model_picker_enabled": true, "policy": { "state": "unconfigured" } },
                    ]
                }),
                200,
                &[],
            )
        }),
        Some(Arc::new(move |model_id| {
            ids_ref.lock().unwrap().push(model_id.to_owned());
            Ok(json_response(
                &json!({ "error": "too many requests" }),
                429,
                &[("Retry-After", "5")],
            ))
        })),
    );

    let store = Arc::new(InMemoryCredentialStore::new());
    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(Arc::clone(&store) as Arc<dyn CredentialStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(github_copilot_provider());

    let interaction = Arc::new(TestCopilotInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        on_device_code: Arc::new(|_| {}),
        on_progress: None,
    });

    let handle = tokio::spawn(async move { models.login("github-copilot", AuthType::OAuth, interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    let credential = handle.await.unwrap().unwrap();

    match &credential {
        Credential::OAuth(oauth) => assert_eq!(oauth.access, TEST_COPILOT_ACCESS_TOKEN),
        other => panic!("expected OAuth credential, got {other:?}"),
    }
    assert_eq!(*policy_model_ids.lock().unwrap(), vec![first_model_id]);
    assert_eq!(store.read("github-copilot", None).await.unwrap(), Some(credential));
}

#[tokio::test]
async fn rejects_a_non_http_verification_uri_before_it_reaches_on_device_code() {
    let _guard = mock_fetch(move |req| {
        Box::pin(async move {
            if req.url.ends_with("/login/device/code") {
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "$(id>/tmp/pwned)",
                        "interval": 1,
                        "expires_in": 900,
                    }),
                    200,
                    &[],
                ));
            }
            panic!("Unexpected fetch URL: {}", req.url);
        })
    });

    let called = Arc::new(Mutex::new(false));
    let called_ref = Arc::clone(&called);
    let err = login_github_copilot_for_test(
        Arc::new(move |_| *called_ref.lock().unwrap() = true),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("Untrusted verification_uri"));
    assert!(!*called.lock().unwrap());
}

#[tokio::test(start_paused = true)]
async fn normalizes_verification_uri_before_it_reaches_on_device_code() {
    set_system_time(parse_date_utc("2026-03-09T00:00:00Z").unwrap());
    let raw_verification_uri = "https://github.com/login/\x1b]8;;evil";
    let normalized_verification_uri = url::Url::parse(raw_verification_uri).unwrap().to_string();
    assert_ne!(normalized_verification_uri, raw_verification_uri);

    let raw_clone = raw_verification_uri.to_owned();
    let _guard = mock_fetch(move |req| {
        let raw_clone = raw_clone.clone();
        Box::pin(async move {
            let url = req.url.as_str();
            if url.ends_with("/login/device/code") {
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": raw_clone,
                        "interval": 1,
                        "expires_in": 900,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/login/oauth/access_token") {
                return Ok(json_response(&json!({ "access_token": "ghu_refresh_token" }), 200, &[]));
            }
            if url.contains("/copilot_internal/v2/token") {
                return Ok(json_response(
                    &json!({
                        "token": "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;",
                        "expires_at": 9999999999_i64,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/models") {
                return Ok(json_response(&json!({ "data": [] }), 200, &[]));
            }
            if url.contains("/models/") && url.ends_with("/policy") {
                return Ok(Response::new(200, Headers::new(), ""));
            }
            panic!("Unexpected fetch URL: {url}");
        })
    });

    let device_codes = Arc::new(Mutex::new(Vec::<RecordedDeviceCode>::new()));
    let dc_ref = Arc::clone(&device_codes);
    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(move |info| dc_ref.lock().unwrap().push(info)),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));

    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert_eq!(
        *device_codes.lock().unwrap(),
        vec![RecordedDeviceCode {
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: normalized_verification_uri,
            interval_seconds: Some(1.0),
            expires_in_seconds: Some(900.0),
        }]
    );

    tokio::time::advance(Duration::from_millis(1000)).await;
    let _ = handle.await.unwrap().unwrap();
    clear_system_time();
}

#[tokio::test(start_paused = true)]
async fn waits_before_polling_and_increases_the_interval_after_slow_down() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    set_system_time(start_ms);

    let access_token_poll_times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let poll_times_ref = Arc::clone(&access_token_poll_times);
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(
            &json!({ "error": "authorization_pending", "error_description": "pending" }),
            200,
            &[],
        ),
        json_response(
            &json!({ "error": "slow_down", "error_description": "slow down", "interval": 7 }),
            200,
            &[],
        ),
        json_response(&json!({ "access_token": "ghu_refresh_token" }), 200, &[]),
    ])));

    let _guard = mock_fetch(move |req| {
        let poll_times_ref = Arc::clone(&poll_times_ref);
        let responses = Arc::clone(&responses);
        Box::pin(async move {
            let url = req.url.as_str();
            if url.ends_with("/login/device/code") {
                assert_eq!(req.method, "POST");
                assert_eq!(req.headers.get("accept").as_deref(), Some("application/json"));
                assert_eq!(
                    req.headers.get("content-type").as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
                let body = req.body_text().unwrap_or_default();
                assert!(body.contains("client_id="));
                assert!(body.contains("scope=read%3Auser"));
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 5,
                        "expires_in": 900,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/login/oauth/access_token") {
                poll_times_ref.lock().unwrap().push(pi_js::time::now_ms());
                assert_eq!(req.method, "POST");
                assert_eq!(req.headers.get("accept").as_deref(), Some("application/json"));
                assert_eq!(
                    req.headers.get("content-type").as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
                let body = req.body_text().unwrap_or_default();
                assert!(body.contains("client_id="));
                assert!(body.contains("device_code=device-code"));
                assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
                let mut guard = responses.lock().unwrap();
                return Ok(guard.pop_front().expect("Unexpected extra access token poll"));
            }
            if url.contains("/copilot_internal/v2/token") {
                return Ok(json_response(
                    &json!({
                        "token": "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;",
                        "expires_at": 9999999999_i64,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/models") {
                return Ok(json_response(&json!({ "data": [] }), 200, &[]));
            }
            if url.contains("/models/") && url.ends_with("/policy") {
                return Ok(Response::new(200, Headers::new(), ""));
            }
            panic!("Unexpected fetch URL: {url}");
        })
    });

    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(|_| {}),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        Some(Arc::new(|_| {})),
    ));

    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 0);

    tokio::time::advance(Duration::from_millis(4999)).await;
    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 0);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 1);

    tokio::time::advance(Duration::from_millis(4999)).await;
    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 2);

    // slow_down carried a server-provided interval of 7 seconds.
    tokio::time::advance(Duration::from_millis(6999)).await;
    tokio::task::yield_now().await;
    assert_eq!(access_token_poll_times.lock().unwrap().len(), 2);

    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = handle.await.unwrap().unwrap();

    assert_eq!(
        *access_token_poll_times.lock().unwrap(),
        vec![start_ms + 5000, start_ms + 10_000, start_ms + 17_000]
    );
    clear_system_time();
}

#[tokio::test(start_paused = true)]
async fn times_out_after_repeated_slow_down_responses() {
    let start_ms = parse_date_utc("2026-03-09T00:00:00Z").unwrap();
    set_system_time(start_ms);

    let access_token_poll_times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let poll_times_ref = Arc::clone(&access_token_poll_times);
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(
            &json!({ "error": "slow_down", "error_description": "slow down" }),
            200,
            &[],
        ),
        json_response(
            &json!({ "error": "slow_down", "error_description": "still too fast" }),
            200,
            &[],
        ),
        json_response(
            &json!({ "error": "authorization_pending", "error_description": "pending" }),
            200,
            &[],
        ),
    ])));

    let _guard = mock_fetch(move |req| {
        let poll_times_ref = Arc::clone(&poll_times_ref);
        let responses = Arc::clone(&responses);
        Box::pin(async move {
            let url = req.url.as_str();
            if url.ends_with("/login/device/code") {
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "interval": 5,
                        "expires_in": 25,
                    }),
                    200,
                    &[],
                ));
            }
            if url.ends_with("/login/oauth/access_token") {
                poll_times_ref.lock().unwrap().push(pi_js::time::now_ms());
                let mut guard = responses.lock().unwrap();
                return Ok(guard.pop_front().expect("Unexpected extra access token poll"));
            }
            panic!("Unexpected fetch URL: {url}");
        })
    });

    let handle = tokio::spawn(login_github_copilot_for_test(
        Arc::new(|_| {}),
        Arc::new(|_, _| Box::pin(async { Ok(String::new()) })),
        None,
    ));

    tokio::time::advance(Duration::from_millis(4999)).await;
    tokio::task::yield_now().await;
    assert!(access_token_poll_times.lock().unwrap().is_empty());

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(*access_token_poll_times.lock().unwrap(), vec![start_ms + 5000]);

    tokio::time::advance(Duration::from_millis(9999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*access_token_poll_times.lock().unwrap(), vec![start_ms + 5000]);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        *access_token_poll_times.lock().unwrap(),
        vec![start_ms + 5000, start_ms + 15_000]
    );

    tokio::time::advance(Duration::from_millis(9999)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        *access_token_poll_times.lock().unwrap(),
        vec![start_ms + 5000, start_ms + 15_000]
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let err = handle.await.unwrap().unwrap_err();
    assert!(
        err.to_string()
            .contains("Device flow timed out after one or more slow_down responses"),
        "unexpected error: {err}"
    );
    assert_eq!(
        *access_token_poll_times.lock().unwrap(),
        vec![start_ms + 5000, start_ms + 15_000]
    );
    clear_system_time();
}

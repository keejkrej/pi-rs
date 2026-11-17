//! Port of packages/ai/test/meta-oauth.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use pi_ai::auth::oauth::meta::meta_oauth;
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, OAuthCredential, ProviderAuthInteraction};
use pi_js::Result;
use pi_js::abort::AbortController;
use pi_js::fetch::Response;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};

const CLIENT_ID: &str = "1031625952748946";
const DEVICE_AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const DEVICE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const MINT_URL: &str = "https://api.meta.ai/muse-code/key";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

struct SystemTimeGuard;
impl Drop for SystemTimeGuard {
    fn drop(&mut self) {
        clear_system_time();
    }
}

struct MockInteraction {
    events: Arc<Mutex<Vec<AuthEvent>>>,
}

#[async_trait::async_trait]
impl AuthInteraction for MockInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Err(pi_js::Error::msg("Meta login should not prompt"))
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap().push(event);
    }
}

fn json_resp(body: serde_json::Value, status: u16) -> Response {
    response(
        status,
        &[("content-type", "application/json")],
        pi_js::json::stringify(&body),
    )
}

#[tokio::test(start_paused = true)]
async fn logs_in_with_device_flow_and_mints_model_api_key() {
    let start_ms = parse_date_utc("2026-09-03T00:00:00Z").expect("valid iso date");
    set_system_time(start_ms);
    let _time_guard = SystemTimeGuard;

    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let poll_responses = Arc::new(Mutex::new(VecDeque::from([
        json_resp(json!({ "error": "authorization_pending" }), 400),
        json_resp(json!({ "access_token": "identity-token", "token_type": "Bearer" }), 200),
    ])));

    let poll_ref = Arc::clone(&poll_responses);
    let _fetch_guard = mock_fetch(move |req| {
        let poll_inner = Arc::clone(&poll_ref);
        Box::pin(async move {
            if req.url == DEVICE_AUTHORIZATION_URL {
                assert_eq!(req.method, "POST");
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(params.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID));
                return Ok(json_resp(
                    json!({
                        "device_code": "device-code-123",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://auth.meta.com/oauth/device/",
                        "verification_uri_complete": "https://auth.meta.com/oauth/device/?code=ABCD-1234",
                        "interval": 5,
                        "expires_in": 600,
                    }),
                    200,
                ));
            }
            if req.url == DEVICE_TOKEN_URL {
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "grant_type" && v == "urn:ietf:params:oauth:grant-type:device_code")
                );
                assert!(params.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID));
                assert!(params.iter().any(|(k, v)| k == "device_code" && v == "device-code-123"));
                return Ok(poll_inner.lock().unwrap().pop_front().expect("poll response"));
            }
            if req.url == MINT_URL {
                assert_eq!(req.method, "POST");
                assert_eq!(
                    req.headers.get("authorization").as_deref(),
                    Some("Bearer identity-token")
                );
                return Ok(json_resp(json!({ "api_key": "LLM|minted-key" }), 200));
            }
            Err(pi_js::Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let oauth = meta_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::clone(&events),
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });

    for _ in 0..5 {
        if !events.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        events.lock().unwrap().first(),
        Some(&AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://auth.meta.com/oauth/device/?code=ABCD-1234".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(600.0),
        })
    );

    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;

    let cred = handle.await.unwrap().expect("meta login succeeds");
    assert_eq!(cred.refresh, "identity-token");
    assert_eq!(cred.access, "LLM|minted-key");
    assert_eq!(cred.expires, start_ms + 10_000 + DAY_MS);
}

#[tokio::test(start_paused = true)]
async fn re_mints_api_key_from_stored_identity_token_on_refresh() {
    let now = parse_date_utc("2026-09-03T12:00:00Z").expect("valid iso date");
    set_system_time(now);
    let _time_guard = SystemTimeGuard;

    let _fetch_guard = mock_fetch(|req| {
        Box::pin(async move {
            assert_eq!(req.url, MINT_URL);
            assert_eq!(
                req.headers.get("authorization").as_deref(),
                Some("Bearer identity-token")
            );
            Ok(json_resp(json!({ "api_key": "LLM|fresh-key" }), 200))
        })
    });

    let oauth = meta_oauth();
    let cred = oauth
        .refresh(
            OAuthCredential::new("identity-token", "LLM|old-key", 1),
            AbortController::new().signal(),
        )
        .await
        .expect("refresh succeeds");
    assert_eq!(cred.refresh, "identity-token");
    assert_eq!(cred.access, "LLM|fresh-key");
    assert_eq!(cred.expires, now + DAY_MS);
}

#[tokio::test]
async fn reports_setup_url_when_meta_issues_no_key() {
    let _fetch_guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(json_resp(
                json!({ "require_payment": true, "action_url": "https://dev.meta.ai/billing" }),
                200,
            ))
        })
    });

    let err = meta_oauth()
        .refresh(
            OAuthCredential::new("identity-token", "", 1),
            AbortController::new().signal(),
        )
        .await
        .expect_err("expected setup error");
    assert!(
        err.to_string()
            .contains("Complete setup at https://dev.meta.ai/billing"),
        "got: {err}"
    );
}

#[tokio::test]
async fn uses_minted_key_as_request_api_key() {
    let auth = meta_oauth()
        .to_auth(OAuthCredential::new("identity-token", "LLM|key", 1))
        .await
        .expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("LLM|key"));
}

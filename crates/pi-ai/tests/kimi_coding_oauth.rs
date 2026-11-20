//! Port of packages/ai/test/kimi-coding-oauth.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use pi_ai::auth::oauth::kimi_coding::kimi_coding_oauth;
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, ModelAuth, OAuthCredential, ProviderAuthInteraction};
use pi_js::Result;
use pi_js::abort::AbortController;
use pi_js::env::testing::EnvGuard;
use pi_js::fetch::Response;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::time::now_ms;
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_HOST: &str = "https://auth.kimi.com";

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
        Err(pi_js::Error::msg("Kimi Code login should not prompt"))
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

fn device_auth_resp(overrides: Option<serde_json::Value>) -> Response {
    let mut base = json!({
        "user_code": "ABCD-1234",
        "device_code": "device-code-123",
        "verification_uri": "https://www.kimi.com/code",
        "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
        "interval": 5,
        "expires_in": 600,
    });
    if let (Some(dst), Some(src)) = (base.as_object_mut(), overrides.as_ref().and_then(|v| v.as_object())) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    json_resp(base, 200)
}

#[tokio::test(start_paused = true)]
async fn logs_in_with_device_authorization_flow() {
    let start_ms = parse_date_utc("2026-07-20T00:00:00Z").expect("valid iso date");
    set_system_time(start_ms);
    let _time_guard = SystemTimeGuard;

    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let poll_times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let poll_responses = Arc::new(Mutex::new(VecDeque::from([
        json_resp(json!({ "error": "authorization_pending" }), 400),
        json_resp(
            json!({
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "expires_in": 3600
            }),
            200,
        ),
    ])));

    let poll_times_ref = Arc::clone(&poll_times);
    let poll_responses_ref = Arc::clone(&poll_responses);
    let _fetch_guard = mock_fetch(move |req| {
        let poll_times_inner = Arc::clone(&poll_times_ref);
        let poll_responses_inner = Arc::clone(&poll_responses_ref);
        Box::pin(async move {
            if req.url == format!("{OAUTH_HOST}/api/oauth/device_authorization") {
                assert_eq!(req.method, "POST");
                assert_eq!(
                    req.headers.get("content-type").as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
                assert_eq!(req.headers.get("accept").as_deref(), Some("application/json"));
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(params.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID));
                return Ok(device_auth_resp(None));
            }
            if req.url == format!("{OAUTH_HOST}/api/oauth/token") {
                poll_times_inner.lock().unwrap().push(now_ms());
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "grant_type" && v == "urn:ietf:params:oauth:grant-type:device_code")
                );
                assert!(params.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID));
                assert!(params.iter().any(|(k, v)| k == "device_code" && v == "device-code-123"));
                let resp = poll_responses_inner
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected extra poll");
                return Ok(resp);
            }
            Err(pi_js::Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let oauth = kimi_coding_oauth();
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
        *events.lock().unwrap(),
        vec![AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://www.kimi.com/code?user_code=ABCD-1234".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(600.0),
            copy_code_on_confirm: None,
            open_browser_on_confirm: None,
        }]
    );

    tokio::time::advance(Duration::from_millis(4999)).await;
    tokio::task::yield_now().await;
    assert!(poll_times.lock().unwrap().is_empty());

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms + 5000]);

    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    let cred = handle.await.unwrap().expect("login succeeds");
    assert_eq!(cred.access, "access-token");
    assert_eq!(cred.refresh, "refresh-token");
    assert_eq!(cred.expires, start_ms + 10_000 + 3600 * 1000);
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms + 5000, start_ms + 10_000]);
}

#[tokio::test(start_paused = true)]
async fn fails_when_device_code_expires() {
    let _fetch_guard = mock_fetch(|req| {
        Box::pin(async move {
            if req.url == format!("{OAUTH_HOST}/api/oauth/device_authorization") {
                return Ok(device_auth_resp(None));
            }
            if req.url == format!("{OAUTH_HOST}/api/oauth/token") {
                return Ok(json_resp(json!({ "error": "expired_token" }), 400));
            }
            Err(pi_js::Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let oauth = kimi_coding_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    let err = handle.await.unwrap().expect_err("expected expired error");
    assert!(err.to_string().contains("expired"), "got: {err}");
}

#[tokio::test(start_paused = true)]
async fn fails_when_user_denies_login() {
    let _fetch_guard = mock_fetch(|req| {
        Box::pin(async move {
            if req.url == format!("{OAUTH_HOST}/api/oauth/device_authorization") {
                return Ok(device_auth_resp(None));
            }
            if req.url == format!("{OAUTH_HOST}/api/oauth/token") {
                return Ok(json_resp(json!({ "error": "access_denied" }), 400));
            }
            Err(pi_js::Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let oauth = kimi_coding_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    let err = handle.await.unwrap().expect_err("expected denied error");
    assert!(err.to_string().contains("denied"), "got: {err}");
}

#[tokio::test(start_paused = true)]
async fn honors_kimi_code_oauth_host_override() {
    let _env_guard = EnvGuard::set("KIMI_CODE_OAUTH_HOST", Some("https://auth.example.com/"));
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);

    let _fetch_guard = mock_fetch(move |req| {
        let urls_inner = Arc::clone(&urls_ref);
        Box::pin(async move {
            urls_inner.lock().unwrap().push(req.url.clone());
            if req.url == "https://auth.example.com/api/oauth/device_authorization" {
                return Ok(device_auth_resp(Some(json!({ "interval": 1 }))));
            }
            if req.url == "https://auth.example.com/api/oauth/token" {
                return Ok(json_resp(
                    json!({ "access_token": "a", "refresh_token": "r", "expires_in": 60 }),
                    200,
                ));
            }
            Err(pi_js::Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let oauth = kimi_coding_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    tokio::task::yield_now().await;
    let cred = handle.await.unwrap().expect("login succeeds");
    assert_eq!(cred.access, "a");
    assert_eq!(cred.refresh, "r");
    assert_eq!(
        *urls.lock().unwrap(),
        vec![
            "https://auth.example.com/api/oauth/device_authorization".to_owned(),
            "https://auth.example.com/api/oauth/token".to_owned(),
        ]
    );
}

#[tokio::test]
async fn refreshes_tokens_and_returns_bearer_header() {
    let _fetch_guard = mock_fetch(|req| {
        Box::pin(async move {
            assert_eq!(req.url, format!("{OAUTH_HOST}/api/oauth/token"));
            let body = req.body_text().unwrap_or_default();
            let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
            assert!(params.iter().any(|(k, v)| k == "grant_type" && v == "refresh_token"));
            assert!(params.iter().any(|(k, v)| k == "refresh_token" && v == "old-refresh"));
            assert!(params.iter().any(|(k, v)| k == "client_id" && v == CLIENT_ID));
            Ok(json_resp(
                json!({ "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600 }),
                200,
            ))
        })
    });

    let oauth = kimi_coding_oauth();
    let before = now_ms();
    let cred = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", before),
            AbortController::new().signal(),
        )
        .await
        .expect("refresh succeeds");
    assert_eq!(cred.access, "new-access");
    assert_eq!(cred.refresh, "new-refresh");
    assert!(cred.expires >= before + 3600 * 1000);

    let auth: ModelAuth = oauth.to_auth(cred).await.expect("to_auth");
    assert_eq!(
        auth.headers
            .as_ref()
            .and_then(|h| h.get("Authorization"))
            .and_then(|v| v.as_deref()),
        Some("Bearer new-access")
    );
}

#[tokio::test(start_paused = true)]
async fn retries_refresh_on_429_and_fails_unauthorized_on_invalid_grant() {
    let calls = Arc::new(Mutex::new(0_usize));
    let calls_ref = Arc::clone(&calls);
    let _fetch_guard = mock_fetch(move |_req| {
        let calls_inner = Arc::clone(&calls_ref);
        Box::pin(async move {
            let mut guard = calls_inner.lock().unwrap();
            *guard += 1;
            if *guard == 1 {
                Ok(json_resp(json!({ "error": "temporarily_unavailable" }), 429))
            } else {
                Ok(json_resp(
                    json!({ "access_token": "a", "refresh_token": "r", "expires_in": 60 }),
                    200,
                ))
            }
        })
    });

    let oauth = kimi_coding_oauth();
    let oauth_clone = Arc::clone(&oauth);
    let handle = tokio::spawn(async move {
        oauth_clone
            .refresh(OAuthCredential::new("old", "old", 0), AbortController::new().signal())
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    tokio::task::yield_now().await;
    let cred = handle.await.unwrap().expect("retried refresh");
    assert_eq!(cred.access, "a");
    assert_eq!(*calls.lock().unwrap(), 2);

    drop(_fetch_guard);
    let _fetch_guard2 = mock_fetch(|_req| Box::pin(async { Ok(json_resp(json!({ "error": "invalid_grant" }), 400)) }));
    let err = oauth
        .refresh(OAuthCredential::new("old", "old", 0), AbortController::new().signal())
        .await
        .expect_err("expected unauthorized");
    assert!(err.to_string().contains("unauthorized"), "got: {err}");
}

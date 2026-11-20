//! Port of packages/ai/test/xai-oauth.test.ts

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use pi_ai::auth::oauth::xai::xai_oauth;
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, OAuthCredential, ProviderAuthInteraction};
use pi_js::Result;
use pi_js::abort::AbortController;
use pi_js::fetch::Response;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::time::now_ms;
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};

struct SystemTimeGuard;
impl Drop for SystemTimeGuard {
    fn drop(&mut self) {
        clear_system_time();
    }
}

struct MockInteraction {
    events: Arc<Mutex<Vec<AuthEvent>>>,
    on_device_code: Option<Arc<dyn Fn(&AuthEvent) + Send + Sync>>,
}

#[async_trait::async_trait]
impl AuthInteraction for MockInteraction {
    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Err(pi_js::Error::msg("Unexpected prompt"))
    }

    fn notify(&self, event: AuthEvent) {
        if let Some(cb) = &self.on_device_code {
            cb(&event);
        }
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

fn device_code_resp(overrides: Option<serde_json::Value>) -> Response {
    let mut base = json!({
        "device_code": "device-code",
        "user_code": "ABCD-1234",
        "verification_uri": "https://accounts.x.ai/oauth2/device",
        "expires_in": 900,
        "interval": 5,
    });
    if let (Some(dst), Some(src)) = (base.as_object_mut(), overrides.as_ref().and_then(|v| v.as_object())) {
        for (k, v) in src {
            if v.is_null() {
                dst.retain(|key, _| key != k);
            } else {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    json_resp(base, 200)
}

fn token_resp(overrides: Option<serde_json::Value>) -> Response {
    let mut base = json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 21_600,
        "token_type": "Bearer",
    });
    if let (Some(dst), Some(src)) = (base.as_object_mut(), overrides.as_ref().and_then(|v| v.as_object())) {
        for (k, v) in src {
            if v.is_null() {
                dst.retain(|key, _| key != k);
            } else {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    json_resp(base, 200)
}

#[tokio::test(start_paused = true)]
async fn uses_device_grant_delays_polling_and_handles_pending_and_slow_down() {
    let start_ms = parse_date_utc("2026-07-09T20:00:00Z").expect("valid iso date");
    set_system_time(start_ms);
    let _time_guard = SystemTimeGuard;

    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let poll_times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let replies = Arc::new(Mutex::new(VecDeque::from([
        json_resp(json!({ "error": "authorization_pending" }), 400),
        json_resp(json!({ "error": "slow_down", "interval": 10 }), 400),
        token_resp(None),
    ])));

    let poll_ref = Arc::clone(&poll_times);
    let replies_ref = Arc::clone(&replies);
    let _fetch_guard = mock_fetch(move |req| {
        let poll_inner = Arc::clone(&poll_ref);
        let replies_inner = Arc::clone(&replies_ref);
        Box::pin(async move {
            if req.url == "https://auth.x.ai/oauth2/device/code" {
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "client_id" && v == "b1a00492-073a-47ea-816f-4c329264a828")
                );
                assert!(params.iter().any(
                    |(k, v)| k == "scope" && v == "openid profile email offline_access grok-cli:access api:access"
                ));
                assert!(params.iter().any(|(k, v)| k == "referrer" && v == "pi"));
                return Ok(device_code_resp(None));
            }
            if req.url == "https://auth.x.ai/oauth2/token" {
                poll_inner.lock().unwrap().push(now_ms());
                let body = req.body_text().unwrap_or_default();
                let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "grant_type" && v == "urn:ietf:params:oauth:grant-type:device_code")
                );
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "client_id" && v == "b1a00492-073a-47ea-816f-4c329264a828")
                );
                assert!(params.iter().any(|(k, v)| k == "device_code" && v == "device-code"));
                return Ok(replies_inner.lock().unwrap().pop_front().expect("token reply"));
            }
            Err(pi_js::Error::msg(format!("Unexpected request: {}", req.url)))
        })
    });

    let oauth = xai_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::clone(&events),
            on_device_code: None,
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });

    tokio::task::yield_now().await;
    assert_eq!(
        *events.lock().unwrap(),
        vec![AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://accounts.x.ai/oauth2/device".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            copy_code_on_confirm: None,
            open_browser_on_confirm: None,
        }]
    );
    assert!(poll_times.lock().unwrap().is_empty());

    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms + 5000]);

    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms + 5000, start_ms + 10_000]);

    tokio::time::advance(Duration::from_millis(10_000)).await;
    tokio::task::yield_now().await;
    let cred = handle.await.unwrap().expect("login succeeds");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![start_ms + 5000, start_ms + 10_000, start_ms + 20_000]
    );
    assert_eq!(cred.access, "access-token");
    assert_eq!(cred.refresh, "refresh-token");
    assert_eq!(cred.expires, start_ms + 20_000 + 21_600_000 - 300_000);
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_default_poll_interval_when_zero() {
    let start_ms = parse_date_utc("2026-07-09T20:00:00Z").expect("valid iso date");
    set_system_time(start_ms);
    let _time_guard = SystemTimeGuard;

    let poll_times = Arc::new(Mutex::new(Vec::<i64>::new()));
    let poll_ref = Arc::clone(&poll_times);
    let _fetch_guard = mock_fetch(move |req| {
        let poll_inner = Arc::clone(&poll_ref);
        Box::pin(async move {
            if req.url == "https://auth.x.ai/oauth2/device/code" {
                return Ok(device_code_resp(Some(json!({ "interval": 0 }))));
            }
            poll_inner.lock().unwrap().push(now_ms());
            Ok(token_resp(None))
        })
    });

    let oauth = xai_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::new(Mutex::new(Vec::new())),
            on_device_code: None,
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    let _ = handle.await.unwrap().expect("login succeeds");
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms + 5000]);
}

#[tokio::test(start_paused = true)]
async fn prefers_verification_uri_complete_and_rejects_untrusted_uris() {
    let _fetch_guard = mock_fetch(|req| {
        Box::pin(async move {
            if req.url == "https://auth.x.ai/oauth2/device/code" {
                return Ok(device_code_resp(Some(json!({
                    "verification_uri_complete": "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"
                }))));
            }
            Ok(token_resp(None))
        })
    });

    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let oauth = xai_oauth();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::clone(&events),
            on_device_code: None,
        }),
        AbortController::new().signal(),
    );
    let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(5000)).await;
    tokio::task::yield_now().await;
    let _ = handle.await.unwrap().expect("login succeeds");
    assert_eq!(
        *events.lock().unwrap(),
        vec![AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            copy_code_on_confirm: None,
            open_browser_on_confirm: None,
        }]
    );

    drop(_fetch_guard);
    for bad_uri in ["http://accounts.x.ai/oauth2/device", "file:///etc/passwd", "not a url"] {
        let bad = bad_uri.to_owned();
        let _bad_guard = mock_fetch(move |_req| {
            let u = bad.clone();
            Box::pin(async move { Ok(device_code_resp(Some(json!({ "verification_uri": u })))) })
        });
        let interaction = ProviderAuthInteraction::new(
            Arc::new(MockInteraction {
                events: Arc::new(Mutex::new(Vec::new())),
                on_device_code: None,
            }),
            AbortController::new().signal(),
        );
        let err = xai_oauth()
            .login(interaction, None)
            .await
            .expect_err("expected untrusted URI rejection");
        assert!(err.to_string().contains("Untrusted verification URI"), "got: {err}");
    }
}

#[tokio::test(start_paused = true)]
async fn fails_when_denied_and_cancels_while_waiting_for_first_poll() {
    for error_code in ["access_denied", "authorization_denied"] {
        let calls = Arc::new(Mutex::new(0_usize));
        let calls_ref = Arc::clone(&calls);
        let code = error_code.to_owned();
        let _guard = mock_fetch(move |_req| {
            let calls_inner = Arc::clone(&calls_ref);
            let c = code.clone();
            Box::pin(async move {
                let mut g = calls_inner.lock().unwrap();
                *g += 1;
                if *g == 1 {
                    Ok(device_code_resp(Some(json!({ "interval": 1 }))))
                } else {
                    Ok(json_resp(json!({ "error": c }), 400))
                }
            })
        });
        let oauth = xai_oauth();
        let interaction = ProviderAuthInteraction::new(
            Arc::new(MockInteraction {
                events: Arc::new(Mutex::new(Vec::new())),
                on_device_code: None,
            }),
            AbortController::new().signal(),
        );
        let handle = tokio::spawn(async move { oauth.login(interaction, None).await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1000)).await;
        tokio::task::yield_now().await;
        let err = handle.await.unwrap().expect_err("expected denied error");
        assert!(
            err.to_string().contains("xAI device authorization was denied"),
            "got: {err}"
        );
    }

    let controller = Arc::new(AbortController::new());
    let controller_cb = Arc::clone(&controller);
    let calls = Arc::new(Mutex::new(0_usize));
    let calls_ref = Arc::clone(&calls);
    let _fetch_guard = mock_fetch(move |_req| {
        *calls_ref.lock().unwrap() += 1;
        Box::pin(async { Ok(device_code_resp(None)) })
    });
    let interaction = ProviderAuthInteraction::new(
        Arc::new(MockInteraction {
            events: Arc::new(Mutex::new(Vec::new())),
            on_device_code: Some(Arc::new(move |_| {
                controller_cb.abort(None);
            })),
        }),
        controller.signal(),
    );
    let err = xai_oauth()
        .login(interaction, None)
        .await
        .expect_err("expected cancellation");
    assert!(err.to_string().contains("Login cancelled"), "got: {err}");
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test(start_paused = true)]
async fn refreshes_tokens_preserves_unrotated_refresh_and_handles_errors() {
    let req_count = Arc::new(Mutex::new(0_usize));
    let req_ref = Arc::clone(&req_count);
    let _fetch_guard = mock_fetch(move |req| {
        let req_inner = Arc::clone(&req_ref);
        Box::pin(async move {
            assert_eq!(req.url, "https://auth.x.ai/oauth2/token");
            let body = req.body_text().unwrap_or_default();
            let params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
            assert!(params.iter().any(|(k, v)| k == "grant_type" && v == "refresh_token"));
            assert!(
                params
                    .iter()
                    .any(|(k, v)| k == "client_id" && v == "b1a00492-073a-47ea-816f-4c329264a828")
            );
            let count = {
                let mut g = req_inner.lock().unwrap();
                *g += 1;
                *g
            };
            if count == 1 {
                assert!(params.iter().any(|(k, v)| k == "refresh_token" && v == "old-refresh"));
                Ok(token_resp(Some(json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh"
                }))))
            } else {
                assert!(params.iter().any(|(k, v)| k == "refresh_token" && v == "keep-refresh"));
                Ok(token_resp(Some(json!({
                    "access_token": "newer-access",
                    "refresh_token": null
                }))))
            }
        })
    });

    let oauth = xai_oauth();
    let rotated = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", 0),
            AbortController::new().signal(),
        )
        .await
        .expect("rotated refresh");
    let preserved = oauth
        .refresh(
            OAuthCredential::new("keep-refresh", "old-access", 0),
            AbortController::new().signal(),
        )
        .await
        .expect("preserved refresh");
    assert_eq!(rotated.refresh, "new-refresh");
    assert_eq!(rotated.access, "new-access");
    assert_eq!(preserved.refresh, "keep-refresh");
    assert_eq!(preserved.access, "newer-access");
    assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
    let auth = oauth.to_auth(preserved).await.expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("newer-access"));

    drop(_fetch_guard);
    let start_ms = parse_date_utc("2026-07-09T20:00:00Z").expect("valid iso date");
    set_system_time(start_ms);
    let _time_guard = SystemTimeGuard;
    let _no_exp_guard = mock_fetch(|_req| Box::pin(async { Ok(token_resp(Some(json!({ "expires_in": null })))) }));
    let fallback_cred = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", 0),
            AbortController::new().signal(),
        )
        .await
        .expect("1h fallback");
    assert_eq!(fallback_cred.expires, start_ms + 3_600_000 - 300_000);

    drop(_no_exp_guard);
    let _missing_guard = mock_fetch(|_req| Box::pin(async { Ok(token_resp(Some(json!({ "access_token": null })))) }));
    let missing_err = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", 0),
            AbortController::new().signal(),
        )
        .await
        .expect_err("missing access_token");
    assert!(
        missing_err
            .to_string()
            .contains("Invalid xAI OAuth response field: access_token"),
        "got: {missing_err}"
    );

    drop(_missing_guard);
    let _revoked_guard = mock_fetch(|_req| {
        Box::pin(async {
            Ok(json_resp(
                json!({ "error": "invalid_grant", "error_description": "refresh token revoked" }),
                400,
            ))
        })
    });
    let revoked_err = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", 0),
            AbortController::new().signal(),
        )
        .await
        .expect_err("revoked error");
    assert!(
        revoked_err
            .to_string()
            .contains("xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked"),
        "got: {revoked_err}"
    );
}

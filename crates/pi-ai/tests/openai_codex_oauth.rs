//! Port of packages/ai/test/openai-codex-oauth.test.ts

use indexmap::IndexMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::auth::oauth::openai_codex::openai_codex_oauth;
use pi_ai::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthSelectOption, OAuthCredential, ProviderAuthInteraction,
};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::b64::btoa;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::time;
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};
use pi_js::{Error, Result};
use serde_json::{Value, json};
use serial_test::serial;
use tokio::net::TcpListener;

struct FakeTimeGuard;

impl FakeTimeGuard {
    fn set(ms: i64) -> Self {
        set_system_time(ms);
        Self
    }
}

impl Drop for FakeTimeGuard {
    fn drop(&mut self) {
        clear_system_time();
    }
}

fn json_resp(body: &Value, status: u16) -> pi_js::fetch::Response {
    response(
        status,
        &[("Content-Type", "application/json")],
        pi_js::json::stringify(body),
    )
}

fn create_access_token(account_id: &str) -> String {
    let header = btoa(&pi_js::json::stringify(&json!({ "alg": "none" }))).unwrap();
    let payload = btoa(&pi_js::json::stringify(&json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id
        }
    })))
    .unwrap();
    format!("{header}.{payload}.signature")
}

fn device_auth_pending_response() -> pi_js::fetch::Response {
    json_resp(
        &json!({
            "error": {
                "message": "Device authorization is pending. Please try again.",
                "type": "invalid_request_error",
                "param": null,
                "code": "deviceauth_authorization_pending"
            }
        }),
        403,
    )
}

#[derive(Clone, Debug, PartialEq)]
struct DeviceInfo {
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
}

struct CodexInteraction<F, N>
where
    F: Fn(AuthPrompt) -> pi_js::BoxFuture<Result<String>> + Send + Sync,
    N: Fn(AuthEvent) + Send + Sync,
{
    prompt_fn: F,
    notify_fn: N,
}

#[async_trait::async_trait]
impl<F, N> AuthInteraction for CodexInteraction<F, N>
where
    F: Fn(AuthPrompt) -> pi_js::BoxFuture<Result<String>> + Send + Sync,
    N: Fn(AuthEvent) + Send + Sync,
{
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        (self.prompt_fn)(prompt).await
    }

    fn notify(&self, event: AuthEvent) {
        (self.notify_fn)(event);
    }
}

fn login_device_code_interaction(
    device_infos: Arc<Mutex<Vec<DeviceInfo>>>,
    signal: AbortSignal,
) -> ProviderAuthInteraction {
    ProviderAuthInteraction::new(
        Arc::new(CodexInteraction {
            prompt_fn: |prompt| {
                Box::pin(async move {
                    if !matches!(prompt.kind, AuthPromptKind::Select { .. }) {
                        return Err(Error::msg("Unexpected prompt"));
                    }
                    Ok("device_code".to_owned())
                })
            },
            notify_fn: move |ev| {
                if let AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                } = ev
                {
                    device_infos.lock().unwrap().push(DeviceInfo {
                        user_code,
                        verification_uri,
                        interval_seconds,
                        expires_in_seconds,
                    });
                }
            },
        }),
        signal,
    )
}

fn parse_form(body: &str) -> IndexMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

#[tokio::test(start_paused = true)]
#[serial]
async fn logs_in_with_the_openai_codex_device_code_flow() {
    let start_ms = parse_date_utc("2026-05-20T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let access_token = create_access_token("account-123");
    let at_c = access_token.clone();
    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let poll_responses = Arc::new(Mutex::new(VecDeque::from([
        device_auth_pending_response(),
        json_resp(
            &json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier"
            }),
            200,
        ),
    ])));

    let pt_c = poll_times.clone();
    let pr_c = poll_responses.clone();
    let _guard = mock_fetch(move |req| {
        let pt = pt_c.clone();
        let pr = pr_c.clone();
        let at = at_c.clone();
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                assert_eq!(req.method, "POST");
                assert_eq!(req.headers.get("content-type").as_deref(), Some("application/json"));
                assert_eq!(
                    req.body_json().unwrap(),
                    json!({ "client_id": "app_EMoamEEZ73f0CkXaXp7hrann" })
                );
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "5"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                pt.lock().unwrap().push(time::now_ms());
                assert_eq!(req.method, "POST");
                assert_eq!(req.headers.get("content-type").as_deref(), Some("application/json"));
                assert_eq!(
                    req.body_json().unwrap(),
                    json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234"
                    })
                );
                return pr
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| Error::msg("Unexpected extra device auth poll"));
            }
            if req.url == "https://auth.openai.com/oauth/token" {
                assert_eq!(req.method, "POST");
                assert_eq!(
                    req.headers.get("content-type").as_deref(),
                    Some("application/x-www-form-urlencoded")
                );
                let params = parse_form(&req.body_text().unwrap());
                assert_eq!(params.get("grant_type").map(String::as_str), Some("authorization_code"));
                assert_eq!(
                    params.get("client_id").map(String::as_str),
                    Some("app_EMoamEEZ73f0CkXaXp7hrann")
                );
                assert_eq!(params.get("code").map(String::as_str), Some("oauth-code"));
                assert_eq!(
                    params.get("redirect_uri").map(String::as_str),
                    Some("https://auth.openai.com/deviceauth/callback")
                );
                assert_eq!(
                    params.get("code_verifier").map(String::as_str),
                    Some("device-code-verifier")
                );
                return Ok(json_resp(
                    &json!({
                        "access_token": at,
                        "refresh_token": "refresh-token",
                        "expires_in": 3600
                    }),
                    200,
                ));
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let di_c = device_infos.clone();
    let sig = AbortController::new().signal();
    let task = tokio::spawn(async move {
        openai_codex_oauth()
            .login(login_device_code_interaction(di_c, sig), None)
            .await
    });

    for _ in 0..8 {
        if !poll_times.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_eq!(
        *device_infos.lock().unwrap(),
        vec![DeviceInfo {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://auth.openai.com/codex/device".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        }]
    );
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(4999)).await;
    tokio::task::yield_now().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms]);

    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..8 {
        if task.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let creds = task.await.unwrap().unwrap();
    assert_eq!(creds.access, access_token);
    assert_eq!(creds.refresh, "refresh-token");
    assert_eq!(creds.expires, start_ms + 5000 + 3600 * 1000);
    assert_eq!(creds.get_extra_str("accountId"), Some("account-123"));
    assert_eq!(*poll_times.lock().unwrap(), vec![start_ms, start_ms + 5000]);
}

#[tokio::test]
#[serial]
async fn offers_browser_login_first_and_uses_the_selected_openai_codex_device_code_flow() {
    let access_token = create_access_token("account-456");
    let at_c = access_token.clone();
    let _guard = mock_fetch(move |req| {
        let at = at_c.clone();
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                assert_eq!(
                    req.body_json().unwrap(),
                    json!({ "client_id": "app_EMoamEEZ73f0CkXaXp7hrann" })
                );
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "WXYZ-7890",
                        "interval": "5"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                return Ok(json_resp(
                    &json!({
                        "authorization_code": "oauth-code",
                        "code_challenge": "device-code-challenge",
                        "code_verifier": "device-code-verifier"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/oauth/token" {
                return Ok(json_resp(
                    &json!({
                        "access_token": at,
                        "refresh_token": "refresh-token",
                        "expires_in": 3600
                    }),
                    200,
                ));
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let select_prompts = Arc::new(Mutex::new(Vec::new()));
    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let sp_c = select_prompts.clone();
    let di_c = device_infos.clone();

    let interaction = ProviderAuthInteraction::new(
        Arc::new(CodexInteraction {
            prompt_fn: move |prompt| {
                let sp = sp_c.clone();
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { message, options } => {
                            sp.lock().unwrap().push((message, options));
                            Ok("device_code".to_owned())
                        }
                        other => panic!("Text prompt should not be used: {other:?}"),
                    }
                })
            },
            notify_fn: move |ev| {
                if let AuthEvent::AuthUrl { .. } = ev {
                    panic!("Browser login should not start");
                }
                if let AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                } = ev
                {
                    di_c.lock().unwrap().push(DeviceInfo {
                        user_code,
                        verification_uri,
                        interval_seconds,
                        expires_in_seconds,
                    });
                }
            },
        }),
        AbortController::new().signal(),
    );

    let creds = openai_codex_oauth().login(interaction, None).await.unwrap();
    assert_eq!(creds.access, access_token);
    assert_eq!(creds.refresh, "refresh-token");
    assert_eq!(creds.get_extra_str("accountId"), Some("account-456"));

    assert_eq!(
        *select_prompts.lock().unwrap(),
        vec![(
            "Select OpenAI Codex login method:".to_owned(),
            vec![
                AuthSelectOption {
                    id: "browser".to_owned(),
                    label: "Browser login (default)".to_owned(),
                    description: None,
                },
                AuthSelectOption {
                    id: "device_code".to_owned(),
                    label: "Device code login (headless)".to_owned(),
                    description: None,
                },
            ],
        )]
    );
    assert_eq!(
        *device_infos.lock().unwrap(),
        vec![DeviceInfo {
            user_code: "WXYZ-7890".to_owned(),
            verification_uri: "https://auth.openai.com/codex/device".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
        }]
    );
}

#[tokio::test]
#[serial]
async fn cancels_when_openai_codex_login_method_selection_is_cancelled() {
    let interaction = ProviderAuthInteraction::new(
        Arc::new(CodexInteraction {
            prompt_fn: |_| Box::pin(async { Err(Error::msg("Login cancelled")) }),
            notify_fn: |_| {},
        }),
        AbortController::new().signal(),
    );
    let err = openai_codex_oauth().login(interaction, None).await.unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
}

#[tokio::test(start_paused = true)]
#[serial]
async fn cancels_the_openai_codex_device_code_flow_while_waiting() {
    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let pt_c = poll_times.clone();
    let _guard = mock_fetch(move |req| {
        let pt = pt_c.clone();
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "5"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                pt.lock().unwrap().push(time::now_ms());
                return Ok(device_auth_pending_response());
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let controller = AbortController::new();
    let sig = controller.signal();
    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn(async move {
        openai_codex_oauth()
            .login(login_device_code_interaction(device_infos, sig), None)
            .await
    });

    for _ in 0..8 {
        if !poll_times.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(poll_times.lock().unwrap().len(), 1);

    controller.abort(None);
    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "Login cancelled");
}

#[tokio::test(start_paused = true)]
#[serial]
async fn times_out_the_openai_codex_device_code_flow_after_15_minutes() {
    let start_ms = parse_date_utc("2026-05-20T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let pt_c = poll_times.clone();
    let _guard = mock_fetch(move |req| {
        let pt = pt_c.clone();
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "60"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                pt.lock().unwrap().push(time::now_ms());
                return Ok(device_auth_pending_response());
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let sig = AbortController::new().signal();
    let task = tokio::spawn(async move {
        openai_codex_oauth()
            .login(login_device_code_interaction(device_infos, sig), None)
            .await
    });

    for _ in 0..8 {
        if !poll_times.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(poll_times.lock().unwrap().len(), 1);

    tokio::time::advance(Duration::from_millis(15 * 60 * 1000)).await;
    for _ in 0..8 {
        if task.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "Device flow timed out");
}

#[tokio::test(start_paused = true)]
#[serial]
async fn treats_openai_codex_device_auth_403_and_404_responses_as_pending() {
    let start_ms = parse_date_utc("2026-05-20T00:00:00Z").unwrap();
    let _clock = FakeTimeGuard::set(start_ms);

    let access_token = create_access_token("account-403-404");
    let at_c = access_token.clone();
    let poll_times = Arc::new(Mutex::new(Vec::new()));
    let poll_responses = Arc::new(Mutex::new(VecDeque::from([
        json_resp(&json!({ "error": "access_denied", "error_description": "denied" }), 403),
        response(404, &[("Content-Type", "text/plain")], "not ready"),
        json_resp(
            &json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier"
            }),
            200,
        ),
    ])));

    let pt_c = poll_times.clone();
    let pr_c = poll_responses.clone();
    let _guard = mock_fetch(move |req| {
        let pt = pt_c.clone();
        let pr = pr_c.clone();
        let at = at_c.clone();
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "1"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                pt.lock().unwrap().push(time::now_ms());
                return pr
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| Error::msg("Unexpected extra device auth poll"));
            }
            if req.url == "https://auth.openai.com/oauth/token" {
                return Ok(json_resp(
                    &json!({
                        "access_token": at,
                        "refresh_token": "refresh-token",
                        "expires_in": 3600
                    }),
                    200,
                ));
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let sig = AbortController::new().signal();
    let task = tokio::spawn(async move {
        openai_codex_oauth()
            .login(login_device_code_interaction(device_infos, sig), None)
            .await
    });

    for _ in 0..8 {
        if !poll_times.lock().unwrap().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(1000)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1000)).await;
    for _ in 0..8 {
        if task.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }

    let creds = task.await.unwrap().unwrap();
    assert_eq!(creds.access, access_token);
    assert_eq!(creds.refresh, "refresh-token");
    assert_eq!(creds.get_extra_str("accountId"), Some("account-403-404"));
    assert_eq!(poll_times.lock().unwrap().len(), 3);
}

#[tokio::test]
#[serial]
async fn includes_the_response_body_in_openai_codex_device_auth_poll_failures() {
    let _guard = mock_fetch(|req| {
        Box::pin(async move {
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Ok(json_resp(
                    &json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "5"
                    }),
                    200,
                ));
            }
            if req.url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                return Ok(json_resp(
                    &json!({ "error": "server_error", "error_description": "try again later" }),
                    500,
                ));
            }
            Err(Error::msg(format!("Unexpected fetch URL: {}", req.url)))
        })
    });

    let device_infos = Arc::new(Mutex::new(Vec::new()));
    let err = openai_codex_oauth()
        .login(
            login_device_code_interaction(device_infos, AbortController::new().signal()),
            None,
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("OpenAI Codex device auth failed with status 500"));
    assert!(msg.contains(r#""error":"server_error""#));
    assert!(msg.contains(r#""error_description":"try again later""#));
}

#[tokio::test]
#[serial]
async fn refresh_surfaces_http_401_error_body_cleanly() {
    let _guard = mock_fetch(|_| {
        Box::pin(async {
            Ok(json_resp(
                &json!({
                    "error": {
                        "message": "Could not validate your token. Please try signing in again.",
                        "type": "invalid_request_error"
                    }
                }),
                401,
            ))
        })
    });

    let err = openai_codex_oauth()
        .refresh(
            OAuthCredential::new("invalid-refresh-token", "invalid-access-token", 0),
            AbortController::new().signal(),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("401") && msg.contains("Could not validate your token"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn falls_back_to_the_pasted_redirect_url_when_the_fixed_callback_port_is_taken() {
    let _blocker = TcpListener::bind("127.0.0.1:1455").await.ok();
    let exchange_body = Arc::new(Mutex::new(None));
    let eb_c = exchange_body.clone();
    let _guard = mock_fetch(move |req| {
        let eb = eb_c.clone();
        Box::pin(async move {
            assert_eq!(req.url, "https://auth.openai.com/oauth/token");
            *eb.lock().unwrap() = Some(parse_form(&req.body_text().unwrap()));
            Ok(json_resp(
                &json!({
                    "access_token": create_access_token("acct"),
                    "refresh_token": "refresh",
                    "expires_in": 3600
                }),
                200,
            ))
        })
    });

    let auth_url = Arc::new(Mutex::new(String::new()));
    let au_n = auth_url.clone();
    let au_p = auth_url.clone();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(CodexInteraction {
            notify_fn: move |ev| {
                if let AuthEvent::AuthUrl { url, .. } = ev {
                    *au_n.lock().unwrap() = url;
                }
            },
            prompt_fn: move |prompt| {
                let au = au_p.clone();
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { .. } => Ok("browser".to_owned()),
                        AuthPromptKind::ManualCode { .. } => {
                            let parsed = url::Url::parse(&au.lock().unwrap()).unwrap();
                            let state = parsed
                                .query_pairs()
                                .find(|(k, _)| k == "state")
                                .map(|(_, v)| v.into_owned())
                                .unwrap_or_default();
                            Ok(format!(
                                "http://localhost:1455/auth/callback?code=pasted-code&state={state}"
                            ))
                        }
                        other => panic!("unexpected prompt: {other:?}"),
                    }
                })
            },
        }),
        AbortController::new().signal(),
    );

    let credential = openai_codex_oauth().login(interaction, None).await.unwrap();
    assert_eq!(credential.get_extra_str("accountId"), Some("acct"));
    let form = exchange_body.lock().unwrap().clone().unwrap();
    assert_eq!(form.get("code").map(String::as_str), Some("pasted-code"));
    assert_eq!(
        form.get("redirect_uri").map(String::as_str),
        Some("http://localhost:1455/auth/callback")
    );
}

//! Port of packages/ai/test/radius-oauth.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use serial_test::serial;

use pi_ai::auth::oauth::radius::{RadiusOAuthOptions, create_radius_oauth};
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, OAuthCredential, ProviderAuthInteraction};
use pi_js::Result;
use pi_js::abort::AbortSignal;
use pi_js::fetch::testing::mock_fetch;
use pi_js::fetch::{Headers, Request, Response, default_client, fetch_with_client};
use pi_js::time::testing::{clear_system_time, parse_date_utc, set_system_time};

const GATEWAY: &str = "https://radius.example";

fn json_response(body: &Value, status: u16) -> Response {
    Response::new(
        status,
        Headers::from_pairs([("content-type", "application/json")]),
        pi_js::json::stringify(body),
    )
}

async fn real_fetch(req: Request) -> Result<Response> {
    fetch_with_client(&default_client(), req).await
}

type NotifyFn = Arc<dyn Fn(AuthEvent) + Send + Sync>;

struct TestRadiusInteraction {
    login_method: String,
    signal: AbortSignal,
    on_notify: NotifyFn,
}

#[async_trait::async_trait]
impl AuthInteraction for TestRadiusInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, _prompt: AuthPrompt) -> Result<String> {
        Ok(self.login_method.clone())
    }

    fn notify(&self, event: AuthEvent) {
        (self.on_notify)(event);
    }
}

#[tokio::test(start_paused = true)]
async fn uses_gateway_endpoints_directly_for_device_login() {
    let start_ms = parse_date_utc("2026-07-24T00:00:00Z").unwrap();
    set_system_time(start_ms);

    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let urls = Arc::new(Mutex::new(Vec::<String>::new()));
    let urls_ref = Arc::clone(&urls);

    let _guard = mock_fetch(move |req| {
        let urls_ref = Arc::clone(&urls_ref);
        Box::pin(async move {
            urls_ref.lock().unwrap().push(req.url.clone());
            let form_raw = req.body_text().unwrap_or_default();
            let form: indexmap::IndexMap<String, String> = url::form_urlencoded::parse(form_raw.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            if req.url == format!("{GATEWAY}/v1/oauth/device") {
                assert_eq!(form.get("client_id").map(String::as_str), Some("pi-gateway"));
                assert_eq!(form.get("scope").map(String::as_str), Some("gateway offline_access"));
                return Ok(json_response(
                    &json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://radius-ui.example/pair",
                        "expires_in": 600,
                        "interval": 5,
                    }),
                    200,
                ));
            }
            if req.url == format!("{GATEWAY}/v1/oauth/token") {
                assert_eq!(
                    form.get("grant_type").map(String::as_str),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(form.get("client_id").map(String::as_str), Some("pi-gateway"));
                assert_eq!(form.get("device_code").map(String::as_str), Some("device-code"));
                return Ok(json_response(
                    &json!({
                        "access_token": "access-token",
                        "refresh_token": "refresh-token",
                        "expires_in": 3600,
                        "scope": "gateway offline_access",
                    }),
                    200,
                ));
            }
            panic!("Unexpected request: {}", req.url);
        })
    });

    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });
    let events_ref = Arc::clone(&events);
    let interaction = Arc::new(TestRadiusInteraction {
        login_method: "device-code".to_owned(),
        signal: AbortSignal::never(),
        on_notify: Arc::new(move |event| events_ref.lock().unwrap().push(event)),
    });

    let cred = oauth
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap();

    assert_eq!(cred.access, "access-token");
    assert_eq!(cred.refresh, "refresh-token");
    assert_eq!(cred.expires, start_ms + 3600 * 1000 - 60_000);
    assert_eq!(cred.get_extra_str("scope"), Some("gateway offline_access"));

    assert_eq!(
        *events.lock().unwrap(),
        vec![AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://radius-ui.example/pair".to_owned(),
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(600.0),
            copy_code_on_confirm: None,
            open_browser_on_confirm: None,
        }]
    );
    assert_eq!(
        *urls.lock().unwrap(),
        vec![
            format!("{GATEWAY}/v1/oauth/device"),
            format!("{GATEWAY}/v1/oauth/token")
        ]
    );
    clear_system_time();
}

#[tokio::test]
async fn refreshes_directly_through_the_gateway_without_discovery() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_ref = Arc::clone(&call_count);

    let _guard = mock_fetch(move |req| {
        let count_ref = Arc::clone(&count_ref);
        Box::pin(async move {
            count_ref.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.url, format!("{GATEWAY}/v1/oauth/token"));
            let form_raw = req.body_text().unwrap_or_default();
            let form: indexmap::IndexMap<String, String> = url::form_urlencoded::parse(form_raw.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            assert_eq!(form.get("grant_type").map(String::as_str), Some("refresh_token"));
            assert_eq!(form.get("client_id").map(String::as_str), Some("pi-gateway"));
            assert_eq!(form.get("refresh_token").map(String::as_str), Some("old-refresh"));
            Ok(json_response(
                &json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 3600,
                }),
                200,
            ))
        })
    });

    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });
    let cred = oauth
        .refresh(
            OAuthCredential::new("old-refresh", "old-access", 0),
            AbortSignal::never(),
        )
        .await
        .unwrap();

    assert_eq!(cred.access, "new-access");
    assert_eq!(cred.refresh, "new-refresh");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_an_authorization_endpoint_that_is_not_an_http_s_url() {
    let _guard = mock_fetch(|_req| {
        Box::pin(async move {
            Ok(json_response(
                &json!({ "authorizationEndpoint": "file:///tmp/payload.app" }),
                200,
            ))
        })
    });
    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let ev_c = Arc::clone(&events);
    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });
    let interaction = Arc::new(TestRadiusInteraction {
        login_method: "browser".to_owned(),
        signal: AbortSignal::never(),
        on_notify: Arc::new(move |ev| ev_c.lock().unwrap().push(ev)),
    });

    let err = oauth
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("Invalid Radius OAuth config from {GATEWAY}"))
    );
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_a_device_verification_uri_that_is_not_an_http_s_url() {
    let _guard = mock_fetch(|_req| {
        Box::pin(async move {
            Ok(json_response(
                &json!({
                    "device_code": "device-code",
                    "user_code": "ABCD-1234",
                    "verification_uri": "file:///tmp/payload.app",
                    "expires_in": 600,
                }),
                200,
            ))
        })
    });
    let events = Arc::new(Mutex::new(Vec::<AuthEvent>::new()));
    let ev_c = Arc::clone(&events);
    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });
    let interaction = Arc::new(TestRadiusInteraction {
        login_method: "device-code".to_owned(),
        signal: AbortSignal::never(),
        on_notify: Arc::new(move |ev| ev_c.lock().unwrap().push(ev)),
    });

    let err = oauth
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Untrusted verification_uri"));
    assert!(events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discovers_only_the_interactive_browser_authorization_endpoint() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_ref = Arc::clone(&call_count);

    let _guard = mock_fetch(move |req| {
        let count_ref = Arc::clone(&count_ref);
        Box::pin(async move {
            count_ref.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.url, format!("{GATEWAY}/v1/oauth"));
            Ok(json_response(&json!({ "issuer": "https://radius-ui.example" }), 200))
        })
    });

    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });
    let interaction = Arc::new(TestRadiusInteraction {
        login_method: "browser".to_owned(),
        signal: AbortSignal::never(),
        on_notify: Arc::new(|_| {}),
    });

    let err = oauth
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("Invalid Radius OAuth config from {GATEWAY}"))
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[serial]
async fn exchanges_the_browser_callback_code_before_showing_the_sign_in_page() {
    let token_status = Arc::new(AtomicUsize::new(400));
    let status_ref = Arc::clone(&token_status);

    let _guard = mock_fetch(move |req| {
        let status_ref = Arc::clone(&status_ref);
        Box::pin(async move {
            if req.url == format!("{GATEWAY}/v1/oauth") {
                return Ok(json_response(
                    &json!({ "authorizationEndpoint": format!("{GATEWAY}/authorize") }),
                    200,
                ));
            }
            if req.url == format!("{GATEWAY}/v1/oauth/token") {
                let form_raw = req.body_text().unwrap_or_default();
                let code = url::form_urlencoded::parse(form_raw.as_bytes())
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.into_owned());
                assert_eq!(code.as_deref(), Some("browser-code"));
                let status = status_ref.load(Ordering::SeqCst) as u16;
                if status == 200 {
                    return Ok(json_response(
                        &json!({
                            "access_token": "access",
                            "refresh_token": "refresh",
                            "expires_in": 3600,
                        }),
                        200,
                    ));
                }
                return Ok(json_response(
                    &json!({ "error": "invalid_grant", "error_description": "code expired" }),
                    status,
                ));
            }
            real_fetch(req).await
        })
    });

    let oauth = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: GATEWAY.to_owned(),
    });

    async fn run_once(oauth: &Arc<dyn pi_ai::auth::types::OAuthAuth>) -> (Result<OAuthCredential>, u16, String) {
        let callback_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<Result<Response>>>));
        let cb_ref = Arc::clone(&callback_handle);
        let interaction = Arc::new(TestRadiusInteraction {
            login_method: "browser".to_owned(),
            signal: AbortSignal::never(),
            on_notify: Arc::new(move |event| {
                let AuthEvent::AuthUrl { url, .. } = event else {
                    return;
                };
                let authorize = url::Url::parse(&url).unwrap();
                let redirect_uri = authorize
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .map(|(_, v)| v.into_owned())
                    .unwrap();
                let state = authorize
                    .query_pairs()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                let mut callback = url::Url::parse(&redirect_uri).unwrap();
                callback
                    .query_pairs_mut()
                    .append_pair("code", "browser-code")
                    .append_pair("state", &state);
                let cb_str = callback.to_string();
                *cb_ref.lock().unwrap() = Some(tokio::spawn(async move { real_fetch(Request::get(cb_str)).await }));
            }),
        });

        let outcome = oauth
            .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
            .await;
        let response = callback_handle.lock().unwrap().take().unwrap().await.unwrap().unwrap();
        let status = response.status();
        let page = response.text().await.unwrap();
        (outcome, status, page)
    }

    let (failed_outcome, failed_status, failed_page) = run_once(&oauth).await;
    let err = failed_outcome.unwrap_err();
    assert!(err.to_string().contains("invalid_grant: code expired"));
    assert_eq!(failed_status, 502);
    assert!(failed_page.contains("code expired"));

    // Allow the closed listener socket to settle on macOS before rebinding fixed port 1456.
    tokio::time::sleep(Duration::from_millis(50)).await;

    token_status.store(200, Ordering::SeqCst);
    let (ok_outcome, ok_status, ok_page) = run_once(&oauth).await;
    assert_eq!(ok_outcome.unwrap().access, "access");
    assert_eq!(ok_status, 200);
    assert!(ok_page.contains("Signed in to Radius."));
}

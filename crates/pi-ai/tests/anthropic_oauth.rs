//! Port of packages/ai/test/anthropic-oauth.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::auth::oauth::anthropic::anthropic_oauth;
use pi_ai::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthSelectOption, ProviderAuthInteraction,
};
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::{Error, Result};
use serde_json::{Value, json};
use serial_test::serial;

fn json_resp(body: &Value) -> pi_js::fetch::Response {
    response(
        200,
        &[("Content-Type", "application/json")],
        pi_js::json::stringify(body),
    )
}

struct TestInteraction<F, N>
where
    F: Fn(AuthPrompt) -> pi_js::BoxFuture<Result<String>> + Send + Sync,
    N: Fn(AuthEvent) + Send + Sync,
{
    prompt_fn: F,
    notify_fn: N,
}

#[async_trait::async_trait]
impl<F, N> AuthInteraction for TestInteraction<F, N>
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

#[tokio::test]
#[serial]
async fn keeps_the_localhost_redirect_uri_for_manual_callback_login() {
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let fc = fetch_calls.clone();
    let _guard = mock_fetch(move |req| {
        let fc2 = fc.clone();
        Box::pin(async move {
            fc2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.url, "https://platform.claude.com/v1/oauth/token");
            assert_eq!(req.method, "POST");
            let body = req.body_json().unwrap();
            assert_eq!(
                body.get("grant_type").and_then(Value::as_str),
                Some("authorization_code")
            );
            assert_eq!(body.get("code").and_then(Value::as_str), Some("manual-code"));
            assert_eq!(
                body.get("redirect_uri").and_then(Value::as_str),
                Some("http://localhost:53692/callback")
            );
            Ok(json_resp(&json!({
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "expires_in": 3600
            })))
        })
    });

    let auth_url = Arc::new(Mutex::new(String::new()));
    let au_notify = auth_url.clone();
    let au_prompt = auth_url.clone();
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(TestInteraction {
            notify_fn: move |ev| {
                if let AuthEvent::AuthUrl { url, .. } = ev {
                    *au_notify.lock().unwrap() = url;
                }
            },
            prompt_fn: move |prompt| {
                let au = au_prompt.clone();
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { .. } => Ok("browser".to_owned()),
                        AuthPromptKind::ManualCode { .. } => {
                            let raw = au.lock().unwrap().clone();
                            let parsed = url::Url::parse(&raw).unwrap();
                            let state = parsed
                                .query_pairs()
                                .find(|(k, _)| k == "state")
                                .map(|(_, v)| v.into_owned())
                                .unwrap();
                            let redirect_uri = parsed
                                .query_pairs()
                                .find(|(k, _)| k == "redirect_uri")
                                .map(|(_, v)| v.into_owned())
                                .unwrap();
                            Ok(format!("{redirect_uri}?code=manual-code&state={state}"))
                        }
                        other => panic!("unexpected prompt: {other:?}"),
                    }
                })
            },
        }),
        sig,
    );

    let credentials = anthropic_oauth().login(interaction, None).await.unwrap();
    assert_eq!(credentials.access, "access-token");
    assert_eq!(credentials.refresh, "refresh-token");
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[serial]
async fn offers_browser_login_first_and_uses_the_selected_anthropic_copy_code_flow() {
    let auth_url = Arc::new(Mutex::new(String::new()));
    let au_fetch = auth_url.clone();
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let fc = fetch_calls.clone();

    let _guard = mock_fetch(move |req| {
        let au = au_fetch.clone();
        let fc2 = fc.clone();
        Box::pin(async move {
            fc2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.url, "https://platform.claude.com/v1/oauth/token");
            let body = req.body_json().unwrap();
            assert_eq!(
                body.get("grant_type").and_then(Value::as_str),
                Some("authorization_code")
            );
            assert_eq!(body.get("code").and_then(Value::as_str), Some("copied-code"));
            let parsed = url::Url::parse(&au.lock().unwrap()).unwrap();
            let expected_state = parsed
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned());
            assert_eq!(
                body.get("state").and_then(Value::as_str).map(str::to_owned),
                expected_state
            );
            assert_eq!(
                body.get("redirect_uri").and_then(Value::as_str),
                Some("https://platform.claude.com/oauth/code/callback")
            );
            Ok(json_resp(&json!({
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "expires_in": 3600
            })))
        })
    });

    let select_prompts = Arc::new(Mutex::new(Vec::new()));
    let sp_c = select_prompts.clone();
    let au_notify = auth_url.clone();
    let au_prompt = auth_url.clone();
    let sig = AbortController::new().signal();

    let interaction = ProviderAuthInteraction::new(
        Arc::new(TestInteraction {
            notify_fn: move |ev| {
                if let AuthEvent::AuthUrl { url, .. } = ev {
                    *au_notify.lock().unwrap() = url;
                }
            },
            prompt_fn: move |prompt| {
                let sp = sp_c.clone();
                let au = au_prompt.clone();
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { message, options } => {
                            sp.lock().unwrap().push((message, options));
                            Ok("copy_code".to_owned())
                        }
                        AuthPromptKind::ManualCode { .. } => {
                            let parsed = url::Url::parse(&au.lock().unwrap()).unwrap();
                            let state = parsed
                                .query_pairs()
                                .find(|(k, _)| k == "state")
                                .map(|(_, v)| v.into_owned())
                                .unwrap();
                            Ok(format!("copied-code#{state}"))
                        }
                        other => panic!("unexpected prompt: {other:?}"),
                    }
                })
            },
        }),
        sig,
    );

    let credentials = anthropic_oauth().login(interaction, None).await.unwrap();
    assert_eq!(credentials.access, "access-token");
    assert_eq!(credentials.refresh, "refresh-token");
    let parsed = url::Url::parse(&auth_url.lock().unwrap()).unwrap();
    assert_eq!(
        parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned())
            .as_deref(),
        Some("https://platform.claude.com/oauth/code/callback")
    );
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *select_prompts.lock().unwrap(),
        vec![(
            "Select Anthropic login method:".to_owned(),
            vec![
                AuthSelectOption {
                    id: "browser".to_owned(),
                    label: "Browser login (default)".to_owned(),
                    description: None,
                },
                AuthSelectOption {
                    id: "copy_code".to_owned(),
                    label: "Copy code login (headless)".to_owned(),
                    description: None,
                },
            ],
        )]
    );
}

#[tokio::test]
#[serial]
async fn cancels_when_anthropic_login_method_selection_is_cancelled() {
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(TestInteraction {
            notify_fn: |_| {},
            prompt_fn: |_| Box::pin(async { Err(Error::msg("Login cancelled")) }),
        }),
        sig,
    );
    let err = anthropic_oauth().login(interaction, None).await.unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
}

#[tokio::test]
#[serial]
async fn omits_scope_from_refresh_token_requests() {
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let fc = fetch_calls.clone();
    let _guard = mock_fetch(move |req| {
        let fc2 = fc.clone();
        Box::pin(async move {
            fc2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.url, "https://platform.claude.com/v1/oauth/token");
            assert_eq!(req.method, "POST");
            let body = req.body_json().unwrap();
            assert_eq!(body.get("grant_type").and_then(Value::as_str), Some("refresh_token"));
            assert!(
                body.get("client_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            );
            assert_eq!(body.get("refresh_token").and_then(Value::as_str), Some("refresh-token"));
            assert!(body.get("scope").is_none());
            Ok(json_resp(&json!({
                "access_token": "new-access-token",
                "refresh_token": "new-refresh-token",
                "expires_in": 3600
            })))
        })
    });

    let sig = AbortController::new().signal();
    let cred = anthropic_oauth()
        .refresh(
            pi_ai::auth::types::OAuthCredential::new("refresh-token", "old-access-token", 0),
            sig,
        )
        .await
        .unwrap();
    assert_eq!(cred.access, "new-access-token");
    assert_eq!(cred.refresh, "new-refresh-token");
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[serial]
async fn anthropic_oauth_login_resolves_through_the_manual_code_prompt_and_aborts_it_after_settling() {
    let _guard = mock_fetch(|req| {
        Box::pin(async move {
            assert!(req.url.contains("/oauth/token"));
            Ok(json_resp(&json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 3600
            })))
        })
    });

    let events = Arc::new(Mutex::new(Vec::new()));
    let has_manual_prompt = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let manual_signal = Arc::new(Mutex::new(None));

    let ev_c = events.clone();
    let mp_c = has_manual_prompt.clone();
    let ms_c = manual_signal.clone();
    let sig = AbortController::new().signal();

    let interaction = ProviderAuthInteraction::new(
        Arc::new(TestInteraction {
            notify_fn: move |ev| ev_c.lock().unwrap().push(ev),
            prompt_fn: move |prompt| {
                let mp = mp_c.clone();
                let ms = ms_c.clone();
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { .. } => Ok("browser".to_owned()),
                        AuthPromptKind::ManualCode { .. } => {
                            mp.store(true, Ordering::SeqCst);
                            *ms.lock().unwrap() = prompt.signal;
                            Ok("the-code".to_owned())
                        }
                        other => panic!("unexpected prompt: {other:?}"),
                    }
                })
            },
        }),
        sig,
    );

    let credential = anthropic_oauth().login(interaction, None).await.unwrap();
    assert_eq!(credential.access, "access");
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, AuthEvent::AuthUrl { .. }))
    );
    assert!(has_manual_prompt.load(Ordering::SeqCst));
    assert!(manual_signal.lock().unwrap().as_ref().unwrap().aborted());
}

#[tokio::test]
#[serial]
async fn completes_login_through_the_browser_callback_and_shows_the_sign_in_page() {
    let exchanged_code = Arc::new(Mutex::new(None));
    let ec_c = exchanged_code.clone();
    let _guard = mock_fetch(move |req| {
        let ec = ec_c.clone();
        Box::pin(async move {
            assert_eq!(req.url, "https://platform.claude.com/v1/oauth/token");
            *ec.lock().unwrap() = req
                .body_json()
                .unwrap()
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(json_resp(&json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 3600
            })))
        })
    });

    let callback_task: Arc<Mutex<Option<tokio::task::JoinHandle<(u16, String)>>>> = Arc::new(Mutex::new(None));
    let cb_c = callback_task.clone();
    let sig = AbortController::new().signal();

    let interaction = ProviderAuthInteraction::new(
        Arc::new(TestInteraction {
            notify_fn: move |ev| {
                if let AuthEvent::AuthUrl { url, .. } = ev {
                    let parsed = url::Url::parse(&url).unwrap();
                    let state = parsed
                        .query_pairs()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    let cb_url = format!("http://127.0.0.1:53692/callback?code=browser-code&state={state}");
                    let h = tokio::spawn(async move {
                        let resp = reqwest::Client::new().get(&cb_url).send().await.unwrap();
                        let status = resp.status().as_u16();
                        let body = resp.text().await.unwrap();
                        (status, body)
                    });
                    *cb_c.lock().unwrap() = Some(h);
                }
            },
            prompt_fn: move |prompt| {
                Box::pin(async move {
                    match prompt.kind {
                        AuthPromptKind::Select { .. } => Ok("browser".to_owned()),
                        _ => {
                            if let Some(s) = prompt.signal {
                                s.cancelled().await;
                            }
                            Err(Error::msg("aborted"))
                        }
                    }
                })
            },
        }),
        sig,
    );

    let credential = anthropic_oauth().login(interaction, None).await.unwrap();
    assert_eq!(credential.access, "access");
    assert_eq!(exchanged_code.lock().unwrap().as_deref(), Some("browser-code"));
    let handle = callback_task.lock().unwrap().take().unwrap();
    let (status, body) = handle.await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("Signed in to Anthropic."));
}

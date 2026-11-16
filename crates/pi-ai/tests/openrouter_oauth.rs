//! Port of packages/ai/test/openrouter-oauth.test.ts

use std::future::pending;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use serial_test::serial;

use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::oauth::openrouter::openrouter_oauth;
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, Credential, ProviderAuthInteraction};
use pi_ai::models::{CreateModelsOptions, create_models};
use pi_ai::providers::openrouter::openrouter_provider;
use pi_ai::types::{AnyModel, ModelType};
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::env::testing::EnvGuard;
use pi_js::fetch::testing::mock_fetch;
use pi_js::fetch::{Headers, Request, Response, default_client, fetch_with_client};
use pi_js::{BoxFuture, Error, Result};

const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const JS_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn json_response(body: &Value, status: u16) -> Response {
    Response::new(
        status,
        Headers::from_pairs([("content-type", "application/json")]),
        pi_js::json::stringify(body),
    )
}

type PromptHandler = Arc<dyn Fn(AuthPrompt) -> BoxFuture<Result<String>> + Send + Sync>;
type NotifyHandler = Arc<dyn Fn(AuthEvent) + Send + Sync>;

struct TestOpenRouterInteraction {
    signal: AbortSignal,
    on_prompt: PromptHandler,
    on_notify: NotifyHandler,
}

#[async_trait::async_trait]
impl AuthInteraction for TestOpenRouterInteraction {
    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.signal)
    }

    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        (self.on_prompt)(prompt).await
    }

    fn notify(&self, event: AuthEvent) {
        (self.on_notify)(event);
    }
}

async fn real_fetch(req: Request) -> Result<Response> {
    fetch_with_client(&default_client(), req).await
}

#[test]
fn is_exposed_alongside_api_key_auth() {
    let provider = openrouter_provider();
    let auth = provider.auth();
    assert!(auth.api_key.is_some());
    let oauth = auth.oauth.as_ref().expect("oauth should be defined");
    assert_eq!(oauth.login_label(), Some("Sign in with OpenRouter"));
}

#[tokio::test]
async fn resolves_the_same_stored_oauth_key_for_chat_and_image_models() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify_with(
            "openrouter",
            |_| async {
                Ok(Some(Credential::OAuth(pi_ai::auth::types::OAuthCredential::new(
                    "",
                    "sk-or-stored",
                    JS_MAX_SAFE_INTEGER,
                ))))
            },
            None,
        )
        .await
        .unwrap();

    let models = create_models(Some(CreateModelsOptions {
        credentials: Some(credentials as Arc<dyn pi_ai::auth::types::CredentialStore + Send + Sync>),
        ..Default::default()
    }));
    models.set_provider(openrouter_provider());

    let chat_model = models
        .get_models(Some("openrouter"))
        .into_iter()
        .next()
        .expect("chat model");
    let image_model = models
        .get_models_of_type(ModelType::Image, Some("openrouter"))
        .into_iter()
        .next()
        .expect("image model");

    assert_eq!(
        models
            .get_auth_model(&AnyModel::Chat(chat_model), None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key)
            .as_deref(),
        Some("sk-or-stored")
    );
    assert_eq!(
        models
            .get_auth_model(&image_model, None)
            .await
            .unwrap()
            .and_then(|r| r.auth.api_key)
            .as_deref(),
        Some("sk-or-stored")
    );
}

#[tokio::test]
async fn runs_pkce_on_a_one_shot_loopback_callback_and_exchanges_code() {
    let exchange_body = Arc::new(Mutex::new(None::<Value>));
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let body_ref = Arc::clone(&exchange_body);
    let calls_ref = Arc::clone(&fetch_calls);

    let _guard = mock_fetch(move |req| {
        let body_ref = Arc::clone(&body_ref);
        let calls_ref = Arc::clone(&calls_ref);
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            calls_ref.fetch_add(1, Ordering::SeqCst);
            *body_ref.lock().unwrap() = req.body_json();
            Ok(json_response(&json!({ "key": "sk-or-test" }), 200))
        })
    });

    let authorize_url = Arc::new(Mutex::new(None::<url::Url>));
    let callback_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<Result<Response>>>));
    let manual_signal = Arc::new(Mutex::new(None::<AbortSignal>));

    let ms_ref = Arc::clone(&manual_signal);
    let auth_ref = Arc::clone(&authorize_url);
    let cb_ref = Arc::clone(&callback_handle);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(move |prompt| {
            *ms_ref.lock().unwrap() = prompt.signal;
            Box::pin(async { pending::<Result<String>>().await })
        }),
        on_notify: Arc::new(move |event| {
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let parsed = url::Url::parse(&url).unwrap();
            let cb_raw = parsed
                .query_pairs()
                .find(|(k, _)| k == "callback_url")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            let mut cb_url = url::Url::parse(&cb_raw).unwrap();
            cb_url.query_pairs_mut().append_pair("code", "authorization-code");
            *auth_ref.lock().unwrap() = Some(parsed);
            let cb_str = cb_url.to_string();
            *cb_ref.lock().unwrap() = Some(tokio::spawn(async move { real_fetch(Request::get(cb_str)).await }));
        }),
    });

    let credential = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap();

    assert_eq!(credential.access, "sk-or-test");
    assert_eq!(credential.refresh, "");
    assert_eq!(credential.expires, JS_MAX_SAFE_INTEGER);

    let cb_resp = callback_handle.lock().unwrap().take().unwrap().await.unwrap().unwrap();
    assert_eq!(cb_resp.status(), 200);
    assert!(manual_signal.lock().unwrap().as_ref().unwrap().aborted());

    let auth_parsed = authorize_url.lock().unwrap().clone().unwrap();
    assert_eq!(auth_parsed.scheme(), "https");
    assert_eq!(auth_parsed.host_str(), Some("openrouter.ai"));
    assert_eq!(auth_parsed.path(), "/auth");
    let query: indexmap::IndexMap<String, String> = auth_parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query.get("code_challenge_method").map(String::as_str), Some("S256"));

    let cb_url = url::Url::parse(query.get("callback_url").unwrap()).unwrap();
    assert_eq!(cb_url.host_str(), Some("127.0.0.1"));
    assert!(cb_url.path().starts_with("/oauth/callback/"));

    let body = exchange_body.lock().unwrap().clone().unwrap();
    assert_eq!(body["code"], "authorization-code");
    assert_eq!(body["code_challenge_method"], "S256");
    let verifier = body["code_verifier"].as_str().unwrap();
    let digest = pi_js::crypto::sha256(verifier.as_bytes());
    let expected_challenge = pi_js::b64::encode_url(&digest);
    assert_eq!(query.get("code_challenge"), Some(&expected_challenge));
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reports_token_exchange_failures_through_both_callback_page_and_login() {
    let _guard = mock_fetch(move |req| {
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            Ok(json_response(&json!({ "error": { "message": "invalid code" } }), 403))
        })
    });

    let callback_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<Result<Response>>>));
    let cb_ref = Arc::clone(&callback_handle);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { pending::<Result<String>>().await })),
        on_notify: Arc::new(move |event| {
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let parsed = url::Url::parse(&url).unwrap();
            let cb_raw = parsed
                .query_pairs()
                .find(|(k, _)| k == "callback_url")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            let mut cb_url = url::Url::parse(&cb_raw).unwrap();
            cb_url.query_pairs_mut().append_pair("code", "bad-code");
            let cb_str = cb_url.to_string();
            *cb_ref.lock().unwrap() = Some(tokio::spawn(async move { real_fetch(Request::get(cb_str)).await }));
        }),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("OpenRouter OAuth key exchange failed (HTTP 403): invalid code")
    );
    let cb_resp = callback_handle.lock().unwrap().take().unwrap().await.unwrap().unwrap();
    assert_eq!(cb_resp.status(), 502);
}

#[tokio::test]
async fn allows_only_one_token_exchange_for_a_callback() {
    let complete_sender = Arc::new(Mutex::new(None::<tokio::sync::oneshot::Sender<Response>>));
    let sender_ref = Arc::clone(&complete_sender);
    let started_notify = Arc::new(tokio::sync::Notify::new());
    let started_ref = Arc::clone(&started_notify);
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&fetch_calls);

    let _guard = mock_fetch(move |req| {
        let sender_ref = Arc::clone(&sender_ref);
        let started_ref = Arc::clone(&started_ref);
        let calls_ref = Arc::clone(&calls_ref);
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            calls_ref.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = tokio::sync::oneshot::channel();
            *sender_ref.lock().unwrap() = Some(tx);
            started_ref.notify_one();
            Ok(rx.await.unwrap())
        })
    });

    let callback_url = Arc::new(Mutex::new(None::<String>));
    let first_callback = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<Result<Response>>>));
    let url_ref = Arc::clone(&callback_url);
    let first_ref = Arc::clone(&first_callback);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { pending::<Result<String>>().await })),
        on_notify: Arc::new(move |event| {
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let parsed = url::Url::parse(&url).unwrap();
            let cb_raw = parsed
                .query_pairs()
                .find(|(k, _)| k == "callback_url")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            let mut cb_url = url::Url::parse(&cb_raw).unwrap();
            cb_url.query_pairs_mut().append_pair("code", "authorization-code");
            let cb_str = cb_url.to_string();
            *url_ref.lock().unwrap() = Some(cb_str.clone());
            *first_ref.lock().unwrap() = Some(tokio::spawn(async move { real_fetch(Request::get(cb_str)).await }));
        }),
    });

    let login_fut = tokio::spawn(async move {
        openrouter_oauth()
            .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
            .await
    });

    started_notify.notified().await;
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);

    let second_cb_url = callback_url.lock().unwrap().clone().unwrap();
    let second_resp = real_fetch(Request::get(second_cb_url)).await.unwrap();
    assert_eq!(second_resp.status(), 409);
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);

    let tx = complete_sender.lock().unwrap().take().unwrap();
    let _ = tx.send(json_response(&json!({ "key": "sk-or-test" }), 200));

    let cred = login_fut.await.unwrap().unwrap();
    assert_eq!(cred.access, "sk-or-test");
    let first_resp = first_callback.lock().unwrap().take().unwrap().await.unwrap().unwrap();
    assert_eq!(first_resp.status(), 200);
}

#[tokio::test]
async fn rejects_a_successful_response_that_does_not_contain_a_key() {
    let _guard = mock_fetch(move |req| {
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            Ok(json_response(&json!({ "user_id": "user-1" }), 200))
        })
    });

    let callback_handle = Arc::new(Mutex::new(None::<tokio::task::JoinHandle<Result<Response>>>));
    let cb_ref = Arc::clone(&callback_handle);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { pending::<Result<String>>().await })),
        on_notify: Arc::new(move |event| {
            let AuthEvent::AuthUrl { url, .. } = event else {
                return;
            };
            let parsed = url::Url::parse(&url).unwrap();
            let cb_raw = parsed
                .query_pairs()
                .find(|(k, _)| k == "callback_url")
                .map(|(_, v)| v.into_owned())
                .unwrap();
            let mut cb_url = url::Url::parse(&cb_raw).unwrap();
            cb_url.query_pairs_mut().append_pair("code", "code-without-key");
            let cb_str = cb_url.to_string();
            *cb_ref.lock().unwrap() = Some(tokio::spawn(async move { real_fetch(Request::get(cb_str)).await }));
        }),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(r#"OpenRouter OAuth response carries no "key""#)
    );
    let cb_resp = callback_handle.lock().unwrap().take().unwrap().await.unwrap().unwrap();
    assert_eq!(cb_resp.status(), 502);
}

#[tokio::test]
async fn mints_a_key_from_a_pasted_redirect_url_when_loopback_never_arrives() {
    let exchange_body = Arc::new(Mutex::new(None::<Value>));
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let body_ref = Arc::clone(&exchange_body);
    let calls_ref = Arc::clone(&fetch_calls);

    let _guard = mock_fetch(move |req| {
        let body_ref = Arc::clone(&body_ref);
        let calls_ref = Arc::clone(&calls_ref);
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            calls_ref.fetch_add(1, Ordering::SeqCst);
            *body_ref.lock().unwrap() = req.body_json();
            Ok(json_response(&json!({ "key": "sk-or-manual" }), 200))
        })
    });

    let callback_url = Arc::new(Mutex::new(None::<String>));
    let cb_ref = Arc::clone(&callback_url);
    let cb_for_notify = Arc::clone(&callback_url);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(move |prompt| {
            let cb_ref = Arc::clone(&cb_ref);
            Box::pin(async move {
                assert!(matches!(
                    prompt.kind,
                    pi_ai::auth::types::AuthPromptKind::ManualCode { .. }
                ));
                let cb = cb_ref.lock().unwrap().clone().unwrap();
                Ok(format!("{cb}?code=manual-code"))
            })
        }),
        on_notify: Arc::new(move |event| {
            if let AuthEvent::AuthUrl { url, .. } = event {
                let parsed = url::Url::parse(&url).unwrap();
                let cb_raw = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "callback_url")
                    .map(|(_, v)| v.into_owned())
                    .unwrap();
                *cb_for_notify.lock().unwrap() = Some(cb_raw);
            }
        }),
    });

    let credential = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap();

    assert_eq!(credential.access, "sk-or-manual");
    assert_eq!(credential.refresh, "");
    assert_eq!(credential.expires, JS_MAX_SAFE_INTEGER);
    let body = exchange_body.lock().unwrap().clone().unwrap();
    assert_eq!(body["code"], "manual-code");
    assert_eq!(body["code_challenge_method"], "S256");
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn accepts_a_bare_authorization_code_from_the_manual_prompt() {
    let exchange_body = Arc::new(Mutex::new(None::<Value>));
    let body_ref = Arc::clone(&exchange_body);

    let _guard = mock_fetch(move |req| {
        let body_ref = Arc::clone(&body_ref);
        Box::pin(async move {
            if req.url != TOKEN_URL {
                return real_fetch(req).await;
            }
            *body_ref.lock().unwrap() = req.body_json();
            Ok(json_response(&json!({ "key": "sk-or-manual" }), 200))
        })
    });

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { Ok("  manual-code  ".to_owned()) })),
        on_notify: Arc::new(|_| {}),
    });

    let credential = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap();
    assert_eq!(credential.access, "sk-or-manual");
    assert_eq!(exchange_body.lock().unwrap().as_ref().unwrap()["code"], "manual-code");
}

#[tokio::test]
async fn fails_login_when_the_manual_prompt_is_cancelled() {
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&fetch_calls);
    let _guard = mock_fetch(move |_| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json_response(&json!({ "key": "sk-or-unexpected" }), 200)) })
    });

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { Err(Error::msg("Login cancelled")) })),
        on_notify: Arc::new(|_| {}),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_empty_manual_input_without_exchanging_a_code() {
    let fetch_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&fetch_calls);
    let _guard = mock_fetch(move |_| {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json_response(&json!({ "key": "sk-or-unexpected" }), 200)) })
    });

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: AbortSignal::never(),
        on_prompt: Arc::new(|_| Box::pin(async { Ok("   ".to_owned()) })),
        on_notify: Arc::new(|_| {}),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, AbortSignal::never()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Missing authorization code"));
    assert_eq!(fetch_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn closes_the_pending_callback_when_login_is_cancelled() {
    let controller = AbortController::new();
    let ctrl_for_notify = controller.clone();
    let callback_url = Arc::new(Mutex::new(None::<String>));
    let cb_ref = Arc::clone(&callback_url);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: controller.signal(),
        on_prompt: Arc::new(|_| Box::pin(async { pending::<Result<String>>().await })),
        on_notify: Arc::new(move |event| {
            if let AuthEvent::AuthUrl { url, .. } = event {
                let parsed = url::Url::parse(&url).unwrap();
                let cb_raw = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "callback_url")
                    .map(|(_, v)| v.into_owned())
                    .unwrap();
                *cb_ref.lock().unwrap() = Some(cb_raw);
                ctrl_for_notify.abort(None);
            }
        }),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, controller.signal()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));

    let cb = callback_url.lock().unwrap().clone().expect("callback URL");
    assert!(real_fetch(Request::get(cb)).await.is_err());
}

#[tokio::test]
async fn rejects_before_opening_a_callback_server_when_login_is_already_cancelled() {
    let controller = AbortController::new();
    controller.abort(None);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: controller.signal(),
        on_prompt: Arc::new(|_| Box::pin(async { Ok(String::new()) })),
        on_notify: Arc::new(|_| panic!("Cancelled login must not emit events")),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, controller.signal()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
}

#[tokio::test]
#[serial]
async fn uses_the_configured_oauth_callback_host() {
    let _env = EnvGuard::set("PI_OAUTH_CALLBACK_HOST", Some("localhost"));
    let controller = AbortController::new();
    let ctrl_for_notify = controller.clone();
    let callback_url = Arc::new(Mutex::new(None::<String>));
    let cb_ref = Arc::clone(&callback_url);

    let interaction = Arc::new(TestOpenRouterInteraction {
        signal: controller.signal(),
        on_prompt: Arc::new(|_| Box::pin(async { pending::<Result<String>>().await })),
        on_notify: Arc::new(move |event| {
            if let AuthEvent::AuthUrl { url, .. } = event {
                let parsed = url::Url::parse(&url).unwrap();
                let cb_raw = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "callback_url")
                    .map(|(_, v)| v.into_owned())
                    .unwrap();
                *cb_ref.lock().unwrap() = Some(cb_raw);
                ctrl_for_notify.abort(None);
            }
        }),
    });

    let err = openrouter_oauth()
        .login(ProviderAuthInteraction::new(interaction, controller.signal()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Login cancelled"));
    let cb = url::Url::parse(callback_url.lock().unwrap().as_ref().unwrap()).unwrap();
    assert_eq!(cb.host_str(), Some("localhost"));
}

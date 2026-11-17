//! Port of packages/ai/test/oauth-callback-server.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::auth::oauth::callback_server::{
    CallbackCompleteFn, CallbackOrManualInput, OAuthCallbackServer, OAuthCallbackServerOptions,
    start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, ProviderAuthInteraction};
use pi_js::abort::AbortController;
use pi_js::{Error, Result};
use tokio::net::TcpListener;
use tokio::sync::Notify;

struct ServerGuard<T: Send + 'static>(Option<OAuthCallbackServer<T>>);

impl<T: Send + 'static> ServerGuard<T> {
    fn new(server: OAuthCallbackServer<T>) -> Self {
        Self(Some(server))
    }

    fn get(&self) -> &OAuthCallbackServer<T> {
        self.0.as_ref().unwrap()
    }
}

impl<T: Send + 'static> Drop for ServerGuard<T> {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.close();
        }
    }
}

fn callback_url(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(redirect_uri).unwrap();
    {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in params {
            pairs.append_pair(k, v);
        }
    }
    url.to_string()
}

struct PageResp {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn get_page(url: &str) -> PageResp {
    let resp = reqwest::Client::new().get(url).send().await.unwrap();
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.text().await.unwrap();
    PageResp {
        status,
        content_type,
        body,
    }
}

async fn start_default<T: Send + 'static>(
    complete: CallbackCompleteFn<T>,
    redirect_host: Option<&str>,
    state: Option<&str>,
    port: u16,
    timeout_ms: Option<u64>,
    signal: Option<pi_js::abort::AbortSignal>,
) -> Result<OAuthCallbackServer<T>> {
    start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Example".to_owned(),
        host: "127.0.0.1".to_owned(),
        port,
        path: "/callback".to_owned(),
        redirect_host: redirect_host.map(str::to_owned),
        state: state.map(str::to_owned),
        complete,
        signal,
        timeout_ms,
    })
    .await
}

struct FnInteraction<F>
where
    F: Fn(AuthPrompt) -> pi_js::BoxFuture<Result<String>> + Send + Sync,
{
    prompt_fn: F,
}

#[async_trait::async_trait]
impl<F> AuthInteraction for FnInteraction<F>
where
    F: Fn(AuthPrompt) -> pi_js::BoxFuture<Result<String>> + Send + Sync,
{
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        (self.prompt_fn)(prompt).await
    }

    fn notify(&self, _event: AuthEvent) {}
}

#[tokio::test]
async fn ignores_stray_requests_and_resolves_with_the_completed_code() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            None,
            Some("expected-state"),
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let redirect = &server.get().redirect_uri;
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));

    let mut other_url = url::Url::parse(redirect).unwrap();
    other_url.set_path("/other");
    let wrong_path = get_page(other_url.as_str()).await;
    assert_eq!(wrong_path.status, 404);

    let wrong_state = get_page(&callback_url(redirect, &[("code", "c"), ("state", "other")])).await;
    assert_eq!(wrong_state.status, 400);
    assert_eq!(wrong_state.content_type.as_deref(), Some("text/html; charset=utf-8"));
    assert!(wrong_state.body.contains("State mismatch."));

    let post_resp = reqwest::Client::new()
        .post(&callback_url(redirect, &[("code", "c"), ("state", "expected-state")]))
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status().as_u16(), 404);

    let missing_code = get_page(&callback_url(redirect, &[("state", "expected-state")])).await;
    assert_eq!(missing_code.status, 400);

    let success = get_page(&callback_url(
        redirect,
        &[("code", "the-code"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(success.status, 200);
    assert_eq!(success.content_type.as_deref(), Some("text/html; charset=utf-8"));
    assert!(success.body.contains("Authentication successful"));
    assert!(success.body.contains("Signed in to Example."));
    assert!(success.body.contains(r##"fill="#F09082""##));
    assert!(success.body.contains(r##"fill="#4D9ABF""##));
    assert!(success.body.contains(r##"fill="#F1BE58""##));
    assert_eq!(
        server.get().wait().await.unwrap(),
        Some("completed:the-code".to_owned())
    );
}

#[tokio::test]
async fn uses_the_redirect_host_and_skips_the_state_check_when_none_is_expected() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            Some("localhost"),
            None,
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let redirect = &server.get().redirect_uri;
    assert!(redirect.starts_with("http://localhost:") && redirect.ends_with("/callback"));
    let resp = get_page(&callback_url(redirect, &[("code", "no-state")])).await;
    assert_eq!(resp.status, 200);
    assert_eq!(
        server.get().wait().await.unwrap(),
        Some("completed:no-state".to_owned())
    );
}

#[tokio::test]
async fn shows_completion_failures_on_the_page_and_rejects_the_wait() {
    let server = ServerGuard::new(
        start_default::<String>(
            Box::new(|_| Box::pin(async { Err(Error::msg("token exchange failed")) })),
            None,
            Some("expected-state"),
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let failure = get_page(&callback_url(
        &server.get().redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(failure.status, 502);
    assert!(failure.body.contains("Example sign-in failed."));
    assert!(failure.body.contains("token exchange failed"));
    let err = server.get().wait().await.unwrap_err();
    assert!(err.to_string().contains("token exchange failed"));
}

#[tokio::test]
async fn rejects_the_wait_when_the_provider_redirects_with_an_error() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            None,
            Some("expected-state"),
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let failure = get_page(&callback_url(
        &server.get().redirect_uri,
        &[
            ("error", "access_denied"),
            ("error_description", "User denied access"),
            ("state", "expected-state"),
        ],
    ))
    .await;
    assert_eq!(failure.status, 400);
    assert!(failure.body.contains("User denied access"));
    let err = server.get().wait().await.unwrap_err();
    assert!(
        err.to_string()
            .contains("Example authorization failed: User denied access")
    );
}

#[tokio::test]
async fn completes_only_the_first_callback() {
    let exchange_started = Arc::new(Notify::new());
    let finish_exchange = Arc::new(Notify::new());
    let es = exchange_started.clone();
    let fe = finish_exchange.clone();

    let server = ServerGuard::new(
        start_default(
            Box::new(move |_| {
                let es2 = es.clone();
                let fe2 = fe.clone();
                Box::pin(async move {
                    es2.notify_waiters();
                    fe2.notified().await;
                    Ok("done".to_owned())
                })
            }),
            None,
            Some("expected-state"),
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let url = callback_url(
        &server.get().redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    );
    let u1 = url.clone();
    let first = tokio::spawn(async move { get_page(&u1).await });
    exchange_started.notified().await;

    let second = get_page(&url).await;
    assert_eq!(second.status, 409);

    // A claimed callback keeps completing even when the caller switches to manual input.
    server.get().cancel();
    finish_exchange.notify_waiters();
    assert_eq!(first.await.unwrap().status, 200);
    assert_eq!(server.get().wait().await.unwrap(), Some("done".to_owned()));
}

#[tokio::test]
async fn resolves_with_undefined_after_cancel() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            None,
            Some("expected-state"),
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    server.get().cancel();
    assert_eq!(server.get().wait().await.unwrap(), None);
    let late = get_page(&callback_url(
        &server.get().redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(late.status, 409);
}

#[tokio::test]
async fn rejects_the_wait_on_abort_and_on_timeout() {
    let controller = AbortController::new();
    let aborted = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            None,
            Some("expected-state"),
            0,
            None,
            Some(controller.signal()),
        )
        .await
        .unwrap(),
    );
    controller.abort(None);
    assert!(
        aborted
            .get()
            .wait()
            .await
            .unwrap_err()
            .to_string()
            .contains("Login cancelled")
    );

    let timed_out = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
            None,
            Some("expected-state"),
            0,
            Some(10),
            None,
        )
        .await
        .unwrap(),
    );
    assert!(
        timed_out
            .get()
            .wait()
            .await
            .unwrap_err()
            .to_string()
            .contains("Example sign-in timed out")
    );

    let already_aborted = AbortController::new();
    already_aborted.abort(None);
    let err = match start_default(
        Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
        None,
        Some("expected-state"),
        0,
        None,
        Some(already_aborted.signal()),
    )
    .await
    {
        Ok(_) => panic!("expected already-aborted server creation to fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Login cancelled"));
}

#[tokio::test]
async fn fails_instead_of_picking_another_port_when_the_requested_port_is_taken() {
    let blocker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = blocker.local_addr().unwrap().port();
    let err = match start_default(
        Box::new(|code| Box::pin(async move { Ok(format!("completed:{code}")) })),
        None,
        Some("expected-state"),
        port,
        None,
        None,
    )
    .await
    {
        Ok(_) => panic!("expected bind on occupied port to fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Failed to bind OAuth callback server"));
}

#[tokio::test]
async fn wait_for_callback_or_manual_input_returns_browser_callback_and_aborts_manual_prompt() {
    let manual_signal = Arc::new(Mutex::new(None));
    let ms_clone = manual_signal.clone();
    let server = Arc::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(code) })),
            None,
            None,
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let redirect = server.redirect_uri.clone();
    let server_for_task = server.clone();
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(FnInteraction {
            prompt_fn: move |prompt: AuthPrompt| {
                let ms = ms_clone.clone();
                Box::pin(async move {
                    let s = prompt.signal.unwrap();
                    *ms.lock().unwrap() = Some(s.clone());
                    s.cancelled().await;
                    Err(Error::msg("prompt aborted"))
                })
            },
        }),
        sig,
    );

    let handle = tokio::spawn(async move {
        wait_for_callback_or_manual_input(
            &interaction,
            Some(server_for_task.as_ref()),
            "paste",
            &server_for_task.redirect_uri,
        )
        .await
    });
    tokio::task::yield_now().await;
    get_page(&callback_url(&redirect, &[("code", "from-browser")])).await;
    let res = handle.await.unwrap().unwrap();
    server.close();
    assert_eq!(res, CallbackOrManualInput::Callback("from-browser".to_owned()));
    assert!(manual_signal.lock().unwrap().as_ref().unwrap().aborted());
}

#[tokio::test]
async fn wait_for_callback_or_manual_input_returns_pasted_input_and_stops_waiting_for_browser() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(code) })),
            None,
            None,
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(FnInteraction {
            prompt_fn: |_| Box::pin(async { Ok("pasted".to_owned()) }),
        }),
        sig,
    );
    let result =
        wait_for_callback_or_manual_input(&interaction, Some(server.get()), "paste", &server.get().redirect_uri)
            .await
            .unwrap();
    assert_eq!(result, CallbackOrManualInput::Manual("pasted".to_owned()));
}

#[tokio::test]
async fn wait_for_callback_or_manual_input_uses_only_manual_prompt_without_callback_server() {
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(FnInteraction {
            prompt_fn: |_| Box::pin(async { Ok("pasted".to_owned()) }),
        }),
        sig,
    );
    let result = wait_for_callback_or_manual_input::<String>(&interaction, None, "paste", "http://localhost/callback")
        .await
        .unwrap();
    assert_eq!(result, CallbackOrManualInput::Manual("pasted".to_owned()));
}

#[tokio::test]
async fn wait_for_callback_or_manual_input_propagates_manual_prompt_failures() {
    let server = ServerGuard::new(
        start_default(
            Box::new(|code| Box::pin(async move { Ok(code) })),
            None,
            None,
            0,
            None,
            None,
        )
        .await
        .unwrap(),
    );
    let sig = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        Arc::new(FnInteraction {
            prompt_fn: |_| Box::pin(async { Err(Error::msg("prompt cancelled")) }),
        }),
        sig,
    );
    let err = wait_for_callback_or_manual_input(&interaction, Some(server.get()), "paste", &server.get().redirect_uri)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("prompt cancelled"));
}
